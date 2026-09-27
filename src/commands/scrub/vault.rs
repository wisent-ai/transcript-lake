//! The literals a scheduled scrub removes: every value Skarbiec holds that could
//! occur as a secret inside transcript text, read through the `skarbiec` CLI as
//! the vault's owner. `list` answers envelope metadata only; `get` answers
//! decrypted fields, which are consumed here and never printed.
//!
//! A value is material when it has at least twelve characters, no line break,
//! and draws on at least three of lower case, upper case, digits and symbols.
//! Skarbiec's item kinds do not say which field is a secret and which names an
//! identity, so every material field counts; an identity that occurs widely is
//! stopped by the scrub's per-literal file cap instead of being rewritten.
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Command;

use serde_json::Value;

use crate::util::{Error, Result};

const MIN_CHARS: usize = 12;
const MIN_CLASSES: usize = 3;

fn skarbiec() -> PathBuf {
    std::env::var_os("SKARBIEC_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".stado/bin/skarbiec")
        })
}

fn material(value: &str) -> bool {
    let classes = [
        value.chars().any(char::is_lowercase),
        value.chars().any(char::is_uppercase),
        value.chars().any(|c| c.is_ascii_digit()),
        value.chars().any(|c| !c.is_alphanumeric()),
    ];
    value.chars().count() >= MIN_CHARS
        && !value.contains('\n')
        && !value.contains('\r')
        && classes.iter().filter(|present| **present).count() >= MIN_CLASSES
}

fn read(vault: &PathBuf, arguments: &[&str]) -> Result<Value> {
    let output = Command::new(skarbiec())
        .args(arguments)
        .env("SKARBIEC_VAULT_FILE", vault)
        .output()
        .map_err(|error| Error(format!("cannot run skarbiec {}: {error}", arguments[0])))?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        return Err(Error(format!(
            "skarbiec {} on {} failed: {}",
            arguments[0],
            vault.display(),
            detail.lines().last().unwrap_or("no detail")
        )));
    }
    serde_json::from_slice(&output.stdout).map_err(|error| {
        Error(format!(
            "skarbiec {} answered unreadable JSON: {error}",
            arguments[0]
        ))
    })
}

pub fn material_literals(vaults: Vec<PathBuf>) -> Result<BTreeSet<String>> {
    let mut values = BTreeSet::new();
    for vault in &vaults {
        if !vault.is_file() {
            return Err(Error(format!("no vault at {}", vault.display())));
        }
        let listing = read(vault, &["list"])?;
        let items = listing.as_array().cloned().unwrap_or_default();
        let (mut contributing, mut unreadable) = (0usize, 0usize);
        for item in &items {
            if item["deleted"].as_bool() == Some(true) {
                continue;
            }
            let Some(id) = item["id"].as_str() else {
                continue;
            };
            let Ok(document) = read(vault, &["get", id]) else {
                unreadable += 1;
                continue;
            };
            let before = values.len();
            for value in document["fields"]
                .as_object()
                .into_iter()
                .flatten()
                .filter_map(|(_, value)| value.as_str())
            {
                if material(value) {
                    values.insert(value.to_owned());
                }
            }
            if values.len() > before {
                contributing += 1;
            }
        }
        println!(
            "vault {} items={} contributing={contributing} unreadable={unreadable}",
            vault.display(),
            items.len()
        );
    }
    Ok(values)
}
