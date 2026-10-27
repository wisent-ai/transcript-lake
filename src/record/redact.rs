//! Secret masker (frozen interface, see the build contract). Every hit is
//! replaced ENTIRELY by a `[masked:<class>:<len>:<fp>]` marker where class is
//! token | entropy | assignment, len is the original hit length, and fp is the
//! leading 32-bit word of the hit's SHA-256 in hex — nothing reversible and no
//! plaintext prefix survives.
//!
//! What counts as a secret is declared, not compiled into this file: a JSON
//! file gives each class its pattern (or `null` to recognise none of that
//! class) and the entropy class its diversity floor. The file
//! `TRANSCRIPT_LAKE_SECRET_FORMATS` names is read when that setting is set;
//! otherwise the declaration that ships with the product
//! (`secret-formats.json` beside this module): the token formats the
//! providers document for their credentials, each recognised by its
//! documented prefix, and no assignment or entropy class, whose lengths and
//! floors nobody stated. A named file that cannot be read, or a declaration
//! with a key missing, refuses the writer by name, so no text reaches the
//! Lake under an unstated rule.
//!
//! Pure string transform once built: no IO, deterministic, idempotent — marker
//! bodies use separators that sit outside every hit alphabet, so a second pass
//! is a no-op as long as no declared pattern matches `[`, `:` or `]`.
use std::path::{Path, PathBuf};

use regex::{Captures, Regex};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::util::{Error, Result};

const FORMATS_ENV: &str = "TRANSCRIPT_LAKE_SECRET_FORMATS";
/// The declaration the product ships, read when the setting names no file.
const SHIPPED: &str = include_str!("secret-formats.json");
/// How the shipped declaration is named in a refusal.
const SHIPPED_NAME: &str = "the secret-format declaration shipped with transcript-lake";

/// The entropy class: candidate runs, kept only when they draw on enough
/// distinct characters and character groups.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EntropyFormat {
    pattern: String,
    min_distinct: usize,
    min_groups: usize,
}

/// The operator's declared secret formats. Every key must be present; `null`
/// declares that the class recognises nothing.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DeclaredFormats {
    #[serde(deserialize_with = "required_nullable")]
    assignment: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    token: Option<String>,
    #[serde(deserialize_with = "required_nullable")]
    entropy: Option<EntropyFormat>,
}

fn required_nullable<'de, D, T>(deserializer: D) -> std::result::Result<Option<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

struct EntropyRule {
    re: Regex,
    min_distinct: usize,
    min_groups: usize,
}

/// Per-class hit counts reported by stream commits and recovery replay.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct MaskCounts {
    pub token: u64,
    pub entropy: u64,
    pub assignment: u64,
}

pub struct Masker {
    assignment: Option<Regex>,
    token: Option<Regex>,
    entropy: Option<EntropyRule>,
    counts: MaskCounts,
}

fn compile(class: &str, pattern: &str, source: &Path) -> Result<Regex> {
    Regex::new(pattern).map_err(|error| {
        Error(format!(
            "{} declares an invalid {class} pattern: {error}",
            source.display()
        ))
    })
}

/// The hit's fingerprint: the leading 32-bit word of its SHA-256, in hex,
/// every byte spelled with both of its digits so equal hits keep one width.
fn fingerprint(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    let hex = format!("{digest:x}");
    let per_byte = hex.len() / digest.len();
    hex[..per_byte * std::mem::size_of::<u32>()].to_string()
}

fn marker(class: &str, value: &str) -> String {
    format!(
        "[masked:{class}:{}:{}]",
        value.chars().count(),
        fingerprint(value)
    )
}

/// Diversity filter for an entropy candidate: at least the declared number of
/// distinct characters, drawn from at least the declared number of character
/// groups (lowercase, uppercase, digit, other).
fn dense_enough(run: &str, rule: &EntropyRule) -> bool {
    let mut distinct: Vec<char> = run.chars().collect();
    distinct.sort_unstable();
    distinct.dedup();
    if distinct.len() < rule.min_distinct {
        return false;
    }
    let has_lower = run.chars().any(|c| c.is_ascii_lowercase());
    let has_upper = run.chars().any(|c| c.is_ascii_uppercase());
    let has_digit = run.chars().any(|c| c.is_ascii_digit());
    let has_other = run.chars().any(|c| !c.is_ascii_alphanumeric());
    let groups = [has_lower, has_upper, has_digit, has_other]
        .into_iter()
        .filter(|hit| *hit)
        .count();
    groups >= rule.min_groups
}

impl Masker {
    /// The masker for the declared formats: the file
    /// `TRANSCRIPT_LAKE_SECRET_FORMATS` names, or the declaration shipped
    /// with the product when the setting is not set.
    pub fn declared() -> Result<Self> {
        let (source, text) = match std::env::var_os(FORMATS_ENV).filter(|value| !value.is_empty()) {
            Some(named) => {
                let source = PathBuf::from(named);
                let text = std::fs::read_to_string(&source).map_err(|error| {
                    Error(format!(
                        "{FORMATS_ENV} file {} cannot be read: {error}",
                        source.display()
                    ))
                })?;
                (source, text)
            }
            None => (PathBuf::from(SHIPPED_NAME), SHIPPED.to_owned()),
        };
        let declared: DeclaredFormats = serde_json::from_str(&text).map_err(|error| {
            Error(format!(
                "{} is not a secret-format declaration: {error}",
                source.display()
            ))
        })?;
        Ok(Self {
            assignment: declared
                .assignment
                .as_deref()
                .map(|pattern| compile("assignment", pattern, &source))
                .transpose()?,
            token: declared
                .token
                .as_deref()
                .map(|pattern| compile("token", pattern, &source))
                .transpose()?,
            entropy: declared
                .entropy
                .map(|format| {
                    Ok::<_, Error>(EntropyRule {
                        re: compile("entropy", &format.pattern, &source)?,
                        min_distinct: format.min_distinct,
                        min_groups: format.min_groups,
                    })
                })
                .transpose()?,
            counts: MaskCounts::default(),
        })
    }

    fn sub(
        text: &str,
        re: &Regex,
        class: &str,
        guard: Option<&EntropyRule>,
        hits: &mut u64,
    ) -> String {
        let mut masked = Vec::new();
        let out = re
            .replace_all(text, |caps: &Captures<'_>| {
                let hit = caps.get_match().as_str();
                if guard.is_some_and(|rule| !dense_enough(hit, rule)) {
                    return hit.to_string();
                }
                masked.push(hit.len());
                marker(class, hit)
            })
            .into_owned();
        *hits += masked.len() as u64;
        out
    }

    /// Order matters: whole assignments first, then provider-shaped tokens,
    /// then leftover dense runs, so each secret is attributed to its richest
    /// class.
    pub fn mask(&mut self, text: &str) -> String {
        if text.is_empty() {
            return String::new();
        }
        let mut out = text.to_string();
        if let Some(re) = &self.assignment {
            out = Self::sub(&out, re, "assignment", None, &mut self.counts.assignment);
        }
        if let Some(re) = &self.token {
            out = Self::sub(&out, re, "token", None, &mut self.counts.token);
        }
        if let Some(rule) = &self.entropy {
            out = Self::sub(
                &out,
                &rule.re,
                "entropy",
                Some(rule),
                &mut self.counts.entropy,
            );
        }
        out
    }

    pub fn counts(&self) -> MaskCounts {
        self.counts
    }
}
