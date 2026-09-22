//! Adapter: Codex CLI rollouts — `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`
//!
//! Frozen interface: `runtime`, `roots(home)`, `list_sessions(root)`, `parser(ctx)`.
//! Adapters emit UNMASKED text (the stream masks) and never do IO in `on_line`;
//! malformed lines are tolerated silently. Envelope per line:
//! `{ timestamp, type, payload }`. Verified on live files, old and current CLI
//! versions: content is duplicated between the response_item stream and event_msg
//! (user_message / agent_message / agent_reasoning). To avoid double counting we take
//! user turns from event_msg/user_message (response_item role=user also carries
//! injected environment context) and everything else from response_item records.
use std::fs;
use std::path::{Path, PathBuf};

use regex::Regex;
use serde_json::Value;

use crate::types::{Adapter, Parser, ParserCtx, SessionEntry};

const TEXT_CAP: usize = 65536;

/// Filenames look like `rollout-<ISO-stamp>-<uuid>.jsonl`; the uuid is the session id.
static UUID_RE: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
    Regex::new("(?i)[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}").expect("uuid pattern")
});

pub struct Codex;

impl Adapter for Codex {
    fn runtime(&self) -> &'static str {
        "codex"
    }

    fn roots(&self, home: &Path) -> Vec<PathBuf> {
        let dir = home.join(".codex").join("sessions");
        if dir.exists() {
            return vec![dir];
        }
        Vec::new()
    }

    fn list_sessions(&self, root: &Path) -> Vec<SessionEntry> {
        let mut sessions = Vec::new();
        for year in subdirs(root) {
            for month in subdirs(&year) {
                for day in subdirs(&month) {
                    for (name, file_type) in read_dirents(&day) {
                        if !file_type.is_file() {
                            continue;
                        }
                        if let Some(session) = day_entry(&day, &name) {
                            sessions.push(session);
                        }
                    }
                }
            }
        }
        sessions
    }

    fn entry_for(&self, path: &Path) -> Option<SessionEntry> {
        let name = path.file_name()?.to_string_lossy();
        let day = path.parent()?;
        let month = day.parent()?;
        let year = month.parent()?;
        let home = crate::util::home_dir();
        if !self
            .roots(&home)
            .iter()
            .any(|root| year.parent() == Some(root.as_path()))
        {
            return None;
        }
        // The date nesting `subdirs` walks is exactly three levels deep and each level
        // must be a real directory, so a rollout parked anywhere else is not ours.
        for dir in [year, month, day] {
            if !dirent_type(dir)?.is_dir() {
                return None;
            }
        }
        if !dirent_type(path)?.is_file() {
            return None;
        }
        day_entry(day, &name)
    }

    fn parser(&self, ctx: ParserCtx) -> Box<dyn Parser> {
        Box::new(CodexParser::new(ctx))
    }
}

/// A directory may vanish between scan and read; that is not an error state worth
/// failing the stream over. Anything else (permissions, IO) was fatal in the previous
/// implementation, which could throw out of `listSessions`; the frozen Rust signature
/// cannot, so it is reported on stderr with the driver's own prefix instead. Names come
/// back in byte order, because Node's `readdirSync` sorts with strcmp and the walk
/// order decides which partition file a record lands in.
fn read_dirents(dir: &Path) -> Vec<(String, fs::FileType)> {
    let iter = match fs::read_dir(dir) {
        Ok(iter) => iter,
        Err(error) => {
            // ENOENT and ENOTDIR: the tree moved under us.
            if !matches!(error.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory) {
                eprintln!(
                    "stream: listSessions failed under {}: {error}",
                    dir.display()
                );
            }
            return Vec::new();
        }
    };
    let mut out = Vec::new();
    for entry in iter.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        out.push((entry.file_name().to_string_lossy().to_string(), file_type));
    }
    out.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    out
}

fn subdirs(dir: &Path) -> Vec<PathBuf> {
    read_dirents(dir)
        .into_iter()
        .filter(|(_, file_type)| file_type.is_dir())
        .map(|(name, _)| dir.join(name))
        .collect()
}

/// The type `read_dirents` would have reported for a path the caller already knows,
/// taken without following a symlink so that an entry the scan skips is skipped here
/// too. A path that vanished between notification and read has no type at all.
fn dirent_type(path: &Path) -> Option<fs::FileType> {
    fs::symlink_metadata(path).ok().map(|meta| meta.file_type())
}

/// The entry a scan yields for one name inside a day directory: `rollout-*.jsonl` only,
/// with the uuid in the stem as the session id. Shared with `entry_for` so one known
/// path and a full scan cannot disagree about a file.
fn day_entry(day: &Path, name: &str) -> Option<SessionEntry> {
    if !name.starts_with("rollout-") || !name.ends_with(".jsonl") {
        return None;
    }
    Some(SessionEntry {
        file: day.join(name),
        session_id: Some(session_id_from_name(name)),
        project: None,
    })
}

fn session_id_from_name(name: &str) -> String {
    let stem = name.strip_suffix(".jsonl").unwrap_or(name);
    match UUID_RE.find(stem) {
        Some(found) => found.as_str().to_string(),
        None => stem.to_string(),
    }
}

/// `String.prototype.slice(0, 65536)`, which counts UTF-16 code units. A cut that would
/// land inside a surrogate pair stops before it rather than emitting a lone surrogate.
pub(super) fn cap(value: &str) -> String {
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

/// A JSON string field, when present and non-empty.
pub(super) fn text_field(value: &Value, key: &str) -> Option<String> {
    match value.get(key) {
        Some(Value::String(text)) if !text.is_empty() => Some(text.clone()),
        _ => None,
    }
}

/// A finite JSON number field, defaulting to zero exactly as `num(x) || 0` did.
pub(super) fn num_or_zero(value: &Value, key: &str) -> i64 {
    value
        .get(key)
        .and_then(Value::as_f64)
        .filter(|raw| raw.is_finite())
        .map_or(0, |raw| raw as i64)
}

mod records;

use records::CodexParser;
