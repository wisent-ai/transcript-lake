//! Discovery and health commands: resolved paths, source availability,
//! dependency checks, and the Lake status snapshot. Everything here reads;
//! neither the state root nor any source store is modified. These four
//! commands are the ones an operator runs before trusting the Lake, so their
//! exit status is meaningful: a broken cursor store, unreadable stream state,
//! or a source that cannot be enumerated is non-zero.
use std::io::Write;
use std::path::PathBuf;

use serde::Serialize;
use serde_json::Value;

use crate::args::{parse_options, require_flags_only};
use crate::paths::lake_paths;
use crate::util::{write_json, Result};

mod doctor;
mod sources;
mod status;

pub use doctor::doctor;
pub use sources::{source_report, sources};
pub use status::{status, status_snapshot, StatusReport};

/// JavaScript `String(value)` for one JSON field: the raw text of a string,
/// the literal spelling of a number or boolean, `null`, and `undefined` for a
/// key the summary never carried. The status lines quote these verbatim, so
/// the rendering has to survive a summary written by an older version.
pub fn js_string(value: Option<&Value>) -> String {
    match value {
        None => "undefined".to_string(),
        Some(Value::Null) => "null".to_string(),
        Some(Value::String(text)) => text.clone(),
        Some(other) => other.to_string(),
    }
}


pub fn paths(rest: &[String]) -> Result<i32> {
    let parsed = parse_options("paths", rest, &[], &["json"])?;
    require_flags_only("paths", &parsed)?;
    let report = lake_paths();
    if parsed.flag("json") {
        write_json(&report)?;
        return Ok(0);
    }
    // The human listing is the JSON object printed one key per line, so the
    // two views cannot drift: an added path shows up in both at once.
    let Value::Object(entries) = serde_json::to_value(&report)? else {
        unreachable!("lake paths always serialize to an object");
    };
    let mut out = std::io::stdout().lock();
    for (name, value) in entries {
        let text = value
            .as_str()
            .filter(|text| !text.is_empty())
            .unwrap_or("not found");
        writeln!(out, "{name}: {text}")?;
    }
    Ok(0)
}

/// One supported source store as `sources` and `doctor` report it.
#[derive(Debug, Serialize)]
pub struct SourceRow {
    pub runtime: String,
    pub available: bool,
    pub mode: &'static str,
    pub roots: Vec<String>,
    #[serde(rename = "sourceIds")]
    pub source_ids: Vec<String>,
    pub selected: bool,
    pub files: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}
