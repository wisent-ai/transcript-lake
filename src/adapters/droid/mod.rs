//! Adapter for Factory Droid session transcripts.
//!
//! Source layout: `HOME/.factory/sessions/<uuid>.jsonl` (legacy, flat) and
//! `HOME/.factory/sessions/<encoded-cwd>/<uuid>.jsonl`, each with an optional
//! `<uuid>.settings.json` sidecar (multi-line JSON: providerLock, tokenUsage).
//! Record types verified across a wide sample of real files on this machine:
//!   session_start — first line, no timestamp; legacy files carry only
//!     `{ id, title, owner }`, newer ones add cwd / version / sessionTitle,
//!   message — `{ id, timestamp, parentId, message: { role, content } }` with
//!     roles user | assistant and content blocks text `{ text }`,
//!     tool_use `{ id, name, input }`, tool_result `{ tool_use_id, content }`,
//!     thinking `{ thinking, signature }`; tool results ride inside user-role
//!     records; no per-message usage or model fields exist,
//!   todo_state — `{ timestamp, todos: { todos: [...] } }`,
//!   compaction_state — `{ timestamp, summaryText }`.
//! Unknown record or block types map to meta events with a type tag in extra
//! rather than being dropped. Sidecars contribute at most one meta event.
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::types::{Adapter, Parser, ParserCtx, SessionEntry};

pub(super) const TEXT_CAP: usize = 65536;
pub(super) const PENDING_CAP: usize = 64;
const JSONL_EXT: &str = ".jsonl";
const SETTINGS_EXT: &str = ".settings.json";

pub struct Droid;

impl Adapter for Droid {
    fn runtime(&self) -> &'static str {
        "droid"
    }

    fn roots(&self, home: &Path) -> Vec<PathBuf> {
        let base = home.join(".factory").join("sessions");
        let Some(entries) = read_dirents(&base) else {
            return Vec::new();
        };
        let mut dirs = vec![base.clone()];
        for (name, file_type) in entries {
            if file_type.is_dir() {
                dirs.push(base.join(name));
            }
        }
        dirs
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
        // Both the flat legacy directory and each encoded-cwd directory beneath it are
        // roots in their own right, so a transcript always sits directly in one.
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
        if ctx.file.to_string_lossy().ends_with(SETTINGS_EXT) {
            return Box::new(SettingsParser::new(ctx));
        }
        Box::new(TranscriptParser::new(ctx))
    }
}

/// Every directory read failure is tolerated here, exactly as the previous
/// implementation's bare `catch { return [] }` did. Names come back in byte order,
/// because Node's `readdirSync` sorts with strcmp and the walk order is observable in
/// `sources` output and in which partition file a record lands in.
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

/// The entry a scan yields for one name inside a session directory: transcripts and
/// their settings sidecars, session id from the stem, project decoded from the
/// directory name. Shared with `entry_for` so one known path and a full scan cannot
/// disagree about a file.
fn session_entry(root: &Path, name: &str) -> Option<SessionEntry> {
    let extension = if name.ends_with(SETTINGS_EXT) {
        SETTINGS_EXT
    } else if name.ends_with(JSONL_EXT) {
        JSONL_EXT
    } else {
        return None;
    };
    Some(SessionEntry {
        file: root.join(name),
        session_id: Some(name[..name.len() - extension.len()].to_string()),
        project: root
            .file_name()
            .and_then(|dir| decode_project(&dir.to_string_lossy())),
    })
}

/// Best-effort decode of '-Users-name-...' directory names; lossy for real
/// dashes in path segments, so session_start cwd overrides it when present.
fn decode_project(name: &str) -> Option<String> {
    if !name.starts_with('-') {
        return None;
    }
    Some(name.replace('-', "/"))
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

/// Sidecar settings files are whole-file JSON, accumulated line by line and

mod settings;
mod transcript;

use settings::SettingsParser;
use transcript::TranscriptParser;
