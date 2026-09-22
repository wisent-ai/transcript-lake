//! Adapter for Oh My Pi (omp) agent session transcripts.
//!
//! Source layout: `HOME/.omp/agent/sessions/<encoded-cwd>/<stamp>_<uuid>.jsonl`;
//! a sibling directory with the same stem holds non-transcript artifacts and
//! is skipped. Typed lines verified on real files from this machine:
//!   session (id, cwd, version), title / title_change (title, updatedAt),
//!   model_change (model), thinking_level_change (thinkingLevel),
//!   message (`{ id, parentId, timestamp, message: { role, content } }`) with
//!     roles user | assistant | toolResult | developer and content blocks
//!     text `{ text }` | thinking `{ thinking }` | toolCall `{ id, name, arguments }`;
//!     toolResult messages carry toolName, toolCallId, isError, content blocks;
//!     assistant messages carry model plus usage `{ input, output, ... }`,
//!   custom_message (customType, content string), custom (customType, data),
//!   compaction (summary, tokensBefore, firstKeptEntryId).
//! Unknown record or block types become meta events tagged in extra.
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::types::{Adapter, Parser, ParserCtx, SessionEntry};

pub(super) const TEXT_CAP: usize = 65536;
pub(super) const PENDING_CAP: usize = 64;
const JSONL_EXT: &str = ".jsonl";

pub struct Omp;

impl Adapter for Omp {
    fn runtime(&self) -> &'static str {
        "omp"
    }

    fn roots(&self, home: &Path) -> Vec<PathBuf> {
        let base = home.join(".omp").join("agent").join("sessions");
        let Some(entries) = read_dirents(&base) else {
            return Vec::new();
        };
        entries
            .into_iter()
            .filter(|(_, file_type)| file_type.is_dir())
            .map(|(name, _)| base.join(name))
            .collect()
    }

    fn list_sessions(&self, root: &Path) -> Vec<SessionEntry> {
        let Some(entries) = read_dirents(root) else {
            return Vec::new();
        };
        let mut sessions = Vec::new();
        for (name, file_type) in entries {
            if !file_type.is_file() {
                continue;
            }
            if let Some(session) = session_entry(root, &name) {
                sessions.push(session);
            }
        }
        sessions
    }

    fn entry_for(&self, path: &Path) -> Option<SessionEntry> {
        let name = path.file_name()?.to_string_lossy();
        let root = path.parent()?;
        let home = crate::util::home_dir();
        // Every omp root is itself an encoded-cwd directory, so a transcript sits
        // directly in one; anything nested deeper was never listed.
        if !self
            .roots(&home)
            .iter()
            .any(|known| known.as_path() == root)
        {
            return None;
        }
        if !dirent_type(path)?.is_file() {
            return None;
        }
        session_entry(root, &name)
    }

    fn parser(&self, ctx: ParserCtx) -> Box<dyn Parser> {
        Box::new(OmpParser::new(ctx))
    }
}

/// Every directory read failure is tolerated here, exactly as the previous
/// implementation's bare `catch { return [] }` did. Names come back in byte order,
/// because Node's `readdirSync` sorts with strcmp and the walk order is observable.
fn read_dirents(dir: &Path) -> Option<Vec<(String, fs::FileType)>> {
    let iter = fs::read_dir(dir).ok()?;
    let mut out = Vec::new();
    for entry in iter.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        out.push((entry.file_name().to_string_lossy().to_string(), file_type));
    }
    out.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    Some(out)
}

/// The type `read_dirents` would have reported for a path the caller already knows,
/// taken without following a symlink so that an entry the scan skips is skipped here
/// too. A path that vanished between notification and read has no type at all.
fn dirent_type(path: &Path) -> Option<fs::FileType> {
    fs::symlink_metadata(path).ok().map(|meta| meta.file_type())
}

/// The entry a scan yields for one name inside a session directory: `.jsonl` files
/// only, session id taken from the `<stamp>_<uuid>` stem. Shared with `entry_for` so
/// one known path and a full scan cannot disagree about a file.
fn session_entry(root: &Path, name: &str) -> Option<SessionEntry> {
    let stem = name.strip_suffix(JSONL_EXT)?;
    // Encoded directory names are home-relative and dash-mangled for omp, so
    // the parser recovers the real project from the session line cwd instead.
    let session_id = match stem.find('_') {
        Some(at) => &stem[at + 1..],
        None => stem,
    };
    Some(SessionEntry {
        file: root.join(name),
        session_id: Some(session_id.to_string()),
        project: None,
    })
}

pub(super) fn clip(value: &str) -> String {
    let mut units = 0usize;
    for (index, character) in value.char_indices() {
        let width = character.len_utf16();
        if units + width > TEXT_CAP {
            return value[..index].to_string();
        }
        units += width;
    }
    value.to_string()
}

pub(super) fn text_of(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(items)) => {
            let mut parts = Vec::new();
            for item in items {
                match item {
                    Value::String(text) => parts.push(text.clone()),
                    Value::Object(map) => {
                        if let Some(Value::String(text)) = map.get("text") {
                            parts.push(text.clone());
                        }
                    }
                    _ => {}
                }
            }
            parts.join("\n")
        }
        _ => String::new(),
    }
}

/// `prune`: a key present with a non-null value survives, everything else is dropped.
pub(super) fn prune(extra: &mut Map<String, Value>, key: &str, value: Option<&Value>) {
    if let Some(value) = value {
        if !value.is_null() {
            extra.insert(key.to_string(), value.clone());
        }
    }
}

/// JS `String(value)` for the shapes a record or block type can take.
pub(super) fn js_string(value: Option<&Value>) -> String {
    match value {
        None => "undefined".to_string(),
        Some(Value::Null) => "null".to_string(),
        Some(Value::Bool(flag)) => flag.to_string(),
        Some(Value::String(text)) => text.clone(),
        Some(Value::Number(number)) => number.to_string(),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| match item {
                Value::Null => String::new(),
                other => js_string(Some(other)),
            })
            .collect::<Vec<_>>()
            .join(","),
        Some(Value::Object(_)) => "[object Object]".to_string(),
    }
}

/// `new Date(ms).toISOString()`: a value outside the representable range threw there
/// and the driver dropped the line, which is what `None` does here.
pub(super) fn epoch_iso(millis: f64) -> Option<String> {
    if !millis.is_finite() {
        return None;
    }
    let stamp = chrono::DateTime::from_timestamp_millis(millis as i64)?;
    Some(stamp.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
}

/// A JSON value that is a number, as `typeof x === 'number'` accepted it.
pub(super) fn number(value: &Value) -> Option<i64> {
    value
        .as_f64()
        .filter(|raw| raw.is_finite())
        .map(|raw| raw as i64)
}

mod records;

use records::OmpParser;
