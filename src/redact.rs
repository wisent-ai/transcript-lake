//! Secret masker (frozen interface, see the build contract). Every hit is
//! replaced ENTIRELY by a `[masked:<class>:<len>:<fp>]` marker where class is
//! token | entropy | assignment, len is the original hit length, and fp is a
//! short prefix of a sha digest of the hit — nothing reversible and no
//! plaintext prefix survives.
//!
//! Pure string transform: no IO, deterministic, idempotent — marker bodies use
//! separators that sit outside every hit alphabet, so a second pass is a no-op.
use std::sync::LazyLock;

use regex::{Captures, Regex};
use serde::Serialize;
use sha2::{Digest, Sha256};

const FP_LEN: usize = 8;
const RUN_MIN: usize = 40;
const DISTINCT_MIN: usize = 16;
const GROUPS_MIN: usize = 3;

/// Dense token alphabet shared by the entropy and assignment value classes.
const DENSE: &str = "[A-Za-z0-9+/=_-]";

// Class (c), assignment shape: an UPPER_CASE name, an equals sign, then a long
// token value. The whole assignment is the hit.
static ASSIGN_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(r"\b[A-Z][A-Z0-9_]{{2,}}={DENSE}{{16,}}")).expect("assignment pattern")
});

// Class (a), provider-token shape: a short lowercase prefix of two to seven
// letters, a dash, then a twenty-plus run of token characters.
static TOKEN_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b[a-z]{2,7}-[A-Za-z0-9_-]{20,}").expect("token pattern"));

// Class (b), candidate dense runs of forty-plus characters; the per-hit
// diversity check below keeps prose and plain hex digests out.
static ENTROPY_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!("{DENSE}{{40,}}")).expect("entropy pattern"));

/// Per-class hit counts reported by stream commits and recovery replay.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct MaskCounts {
    pub token: u64,
    pub entropy: u64,
    pub assignment: u64,
}

#[derive(Debug, Default)]
pub struct Masker {
    counts: MaskCounts,
}

fn fingerprint(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    let hex = format!("{digest:x}");
    hex[..FP_LEN].to_string()
}

fn marker(class: &str, value: &str) -> String {
    format!(
        "[masked:{class}:{}:{}]",
        value.chars().count(),
        fingerprint(value)
    )
}

/// High-entropy filter: long enough, many distinct characters, and drawing on
/// several character groups at once. Lowercase prose (one group) and plain hex
/// digests (two groups, sixteen distinct at most) both fail this on purpose.
fn dense_enough(run: &str) -> bool {
    if run.chars().count() < RUN_MIN {
        return false;
    }
    let mut distinct: Vec<char> = run.chars().collect();
    distinct.sort_unstable();
    distinct.dedup();
    if distinct.len() < DISTINCT_MIN {
        return false;
    }
    let has_lower = run.chars().any(|c| c.is_ascii_lowercase());
    let has_upper = run.chars().any(|c| c.is_ascii_uppercase());
    let has_digit = run.chars().any(|c| c.is_ascii_digit());
    let has_symbol = run
        .chars()
        .any(|c| matches!(c, '+' | '/' | '=' | '_' | '-'));
    let groups = [has_lower, has_upper, has_digit, has_symbol]
        .into_iter()
        .filter(|hit| *hit)
        .count();
    groups >= GROUPS_MIN
}

impl Masker {
    pub fn new() -> Self {
        Self::default()
    }

    fn sub(&mut self, text: &str, re: &Regex, class: &str, guarded: bool) -> String {
        let mut hits = 0u64;
        let out = re.replace_all(text, |caps: &Captures<'_>| {
            let hit = &caps[0];
            if guarded && !dense_enough(hit) {
                return hit.to_string();
            }
            hits += 1;
            marker(class, hit)
        });
        match class {
            "token" => self.counts.token += hits,
            "entropy" => self.counts.entropy += hits,
            _ => self.counts.assignment += hits,
        }
        out.into_owned()
    }

    /// Order matters: whole assignments first, then provider-shaped tokens,
    /// then leftover dense runs, so each secret is attributed to its richest
    /// class.
    pub fn mask(&mut self, text: &str) -> String {
        if text.is_empty() {
            return String::new();
        }
        let out = self.sub(text, &ASSIGN_RE, "assignment", false);
        let out = self.sub(&out, &TOKEN_RE, "token", false);
        self.sub(&out, &ENTROPY_RE, "entropy", true)
    }

    pub fn counts(&self) -> MaskCounts {
        self.counts
    }
}
