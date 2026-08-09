//! Flag parsing with the same shape, and the same error sentences, as the
//! previous implementation: long flags only, duplicates rejected, a value flag
//! never swallows the next flag, and anything unknown is a hard error.
use std::collections::HashMap;

use crate::types::SUPPORTED_SOURCES;
use crate::util::{Error, Result};

pub const DEFAULT_LIMIT: i64 = 20;
pub const MAX_LIMIT: i64 = 500;
pub const DEFAULT_DAYS: i64 = 7;
pub const DEFAULT_DEBOUNCE: u64 = 60;
pub const SHOW_LIMIT: i64 = 2000;
pub const SHOW_MAX_LIMIT: i64 = 50000;

#[derive(Debug, Default)]
pub struct Parsed {
    values: HashMap<String, String>,
    flags: HashMap<String, bool>,
    pub positionals: Vec<String>,
}

impl Parsed {
    /// Value of `--name`, if given.
    pub fn value(&self, name: &str) -> Option<&str> {
        self.values.get(name).map(String::as_str)
    }

    /// Whether the boolean `--name` was given.
    pub fn flag(&self, name: &str) -> bool {
        self.flags.get(name).copied().unwrap_or(false)
    }
}

/// Parse `rest` against the value flags and boolean flags a command accepts.
/// Flag names are given without the leading dashes.
pub fn parse_options(
    command: &str,
    rest: &[String],
    value_flags: &[&str],
    boolean_flags: &[&str],
) -> Result<Parsed> {
    let mut parsed = Parsed::default();
    let mut queue = rest.iter();
    while let Some(token) = queue.next() {
        let Some(name) = token.strip_prefix("--") else {
            parsed.positionals.push(token.clone());
            continue;
        };
        if boolean_flags.contains(&name) {
            if parsed.flags.contains_key(name) {
                return Err(Error(format!("{command} received duplicate {token}")));
            }
            parsed.flags.insert(name.to_string(), true);
            continue;
        }
        if value_flags.contains(&name) {
            if parsed.values.contains_key(name) {
                return Err(Error(format!("{command} received duplicate {token}")));
            }
            let value = queue.next();
            match value {
                Some(value) if !value.is_empty() && !value.starts_with("--") => {
                    parsed.values.insert(name.to_string(), value.clone());
                }
                _ => return Err(Error(format!("{token} requires a value"))),
            }
            continue;
        }
        return Err(Error(format!("unknown {command} flag: {token}")));
    }
    Ok(parsed)
}

/// A bounded positive integer flag, or the fallback when the flag is absent.
pub fn bounded_integer(
    value: Option<&str>,
    name: &str,
    fallback: i64,
    maximum: i64,
) -> Result<i64> {
    let Some(value) = value else {
        return Ok(fallback);
    };
    match value.parse::<i64>() {
        Ok(parsed) if parsed >= 1 && parsed <= maximum => Ok(parsed),
        _ => Err(Error(format!(
            "{name} must be an integer from 1 to {maximum}"
        ))),
    }
}

/// Validate `--runtime`/`--source` against the supported runtimes.
pub fn require_runtime(value: Option<&str>) -> Result<Option<String>> {
    let Some(value) = value else {
        return Ok(None);
    };
    if !SUPPORTED_SOURCES.contains(&value) {
        return Err(Error(format!(
            "unknown source \"{value}\" (expected one of: {})",
            SUPPORTED_SOURCES.join(", ")
        )));
    }
    Ok(Some(value.to_string()))
}

/// Reject any argument for a command that takes none.
pub fn require_no_args(command: &str, rest: &[String]) -> Result<()> {
    if rest.is_empty() {
        return Ok(());
    }
    Err(Error(format!("{command} accepts no arguments or flags")))
}

/// Reject positionals for a command that takes flags only.
pub fn require_flags_only(command: &str, parsed: &Parsed) -> Result<()> {
    if parsed.positionals.is_empty() {
        return Ok(());
    }
    Err(Error(format!("{command} accepts flags only")))
}
