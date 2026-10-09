//! Flag parsing with the same shape, and the same error sentences, as the
//! previous implementation: long flags only, duplicates rejected, a value flag
//! never swallows the next flag, and anything unknown is a hard error.
use std::collections::HashMap;

use crate::types::supported_sources;
use crate::util::{Error, Result};

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

/// A positive integer flag, or `None` when the flag is absent. Zero and
/// anything that is not a positive integer are refused by name, with the
/// sentence that says what omitting the flag does.
pub fn optional_positive(value: Option<&str>, name: &str, omitted: &str) -> Result<Option<i64>> {
    let Some(value) = value else {
        return Ok(None);
    };
    match value.parse::<i64>() {
        Ok(parsed) if parsed >= 1 => Ok(Some(parsed)),
        _ => Err(Error(format!(
            "{name} must be a whole number of at least one; omit it to {omitted}"
        ))),
    }
}

/// The SQL `LIMIT` clause for an optional `--limit`: empty when the flag is
/// absent, so every row is returned.
pub fn limit_clause(value: Option<&str>, omitted: &str) -> Result<String> {
    Ok(optional_positive(value, "--limit", omitted)?
        .map(|limit| format!(" LIMIT {limit}"))
        .unwrap_or_default())
}

/// Validate `--runtime`/`--source` against the supported runtimes.
pub fn require_runtime(value: Option<&str>) -> Result<Option<String>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let supported = supported_sources();
    if !supported.contains(&value) {
        return Err(Error(format!(
            "unknown source \"{value}\" (expected one of: {}; `transcript-lake sources` shows each with its roots on this machine)",
            supported.join(", ")
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
