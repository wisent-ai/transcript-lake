//! Adapter: Claude Code transcripts — `~/.claude/projects/<encoded-cwd>/<sessionId>.jsonl`
//!
//! Frozen interface: `runtime`, `roots(home)`, `list_sessions(root)`, `parser(ctx)`.
//! Adapters emit UNMASKED text (the stream masks) and never do IO in `on_line`.
//! Contract: malformed lines are tolerated silently (no events). Verified against live
//! files on this machine: record types user | assistant | system | summary carry
//! messages; permission-mode, file-history-snapshot, attachment, ai-title, last-prompt,
//! queue-operation, progress and mode records are bookkeeping noise and are dropped.
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::types::{Adapter, Parser, ParserCtx, SessionEntry};

const TEXT_CAP: usize = 65536;

pub struct Claude;

impl Adapter for Claude {
    fn runtime(&self) -> &'static str {
        "claude"
    }

    fn roots(&self, home: &Path) -> Vec<PathBuf> {
        let dir = home.join(".claude").join("projects");
        if dir.exists() {
            return vec![dir];
        }
        Vec::new()
    }

    fn list_sessions(&self, root: &Path) -> Vec<SessionEntry> {
        let mut sessions = Vec::new();
        for entry in read_dirents(root) {
            if !entry.1.is_dir() {
                continue;
            }
            let project_dir = root.join(&entry.0);
            for child in read_dirents(&project_dir) {
                if !child.1.is_file() {
                    continue;
                }
                if let Some(session) = project_entry(&project_dir, &child.0) {
                    sessions.push(session);
                }
            }
        }
        sessions
    }

    fn entry_for(&self, path: &Path) -> Option<SessionEntry> {
        let name = path.file_name()?.to_string_lossy();
        let project_dir = path.parent()?;
        let home = crate::util::home_dir();
        if !self
            .roots(&home)
            .iter()
            .any(|root| project_dir.parent() == Some(root.as_path()))
        {
            return None;
        }
        if !dirent_type(project_dir)?.is_dir() || !dirent_type(path)?.is_file() {
            return None;
        }
        project_entry(project_dir, &name)
    }

    fn parser(&self, ctx: ParserCtx) -> Box<dyn Parser> {
        Box::new(ClaudeParser::new(ctx))
    }
}

/// A root or project directory may vanish between scan and read; that is not an error
/// state worth failing the stream over. Anything else (permissions, IO) was fatal in the
/// previous implementation, which could throw out of `listSessions`; the frozen Rust
/// signature cannot, so it is reported on stderr with the driver's own prefix instead.
/// Names come back in byte order, because Node's `readdirSync` sorts with strcmp and
/// the walk order decides which partition file a record lands in.
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

/// The type `read_dirents` would have reported for a path the caller already knows,
/// taken without following a symlink so that an entry the scan skips is skipped here
/// too. A path that vanished between notification and read has no type at all.
fn dirent_type(path: &Path) -> Option<fs::FileType> {
    fs::symlink_metadata(path).ok().map(|meta| meta.file_type())
}

/// Directory names encode the cwd with '/' turned into '-'; the reverse mapping is best
/// effort (dashes that belonged to the real path are indistinguishable). The parser
/// prefers the per-record cwd field over this value whenever one is present.
fn decode_project_dir(name: &str) -> Option<String> {
    if !name.starts_with('-') {
        return None;
    }
    Some(name.replace('-', "/"))
}

/// The entry a scan yields for one name inside a project directory: `.jsonl` files
/// only, session id from the stem, project decoded from the directory name. Shared
/// with `entry_for` so one known path and a full scan cannot disagree about a file.
fn project_entry(project_dir: &Path, name: &str) -> Option<SessionEntry> {
    let session_id = name.strip_suffix(".jsonl")?;
    Some(SessionEntry {
        file: project_dir.join(name),
        session_id: Some(session_id.to_string()),
        project: project_dir
            .file_name()
            .and_then(|dir| decode_project_dir(&dir.to_string_lossy())),
    })
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

/// A finite JSON number field.
pub(super) fn num_field(value: &Value, key: &str) -> Option<i64> {
    let raw = value.get(key).and_then(Value::as_f64)?;
    raw.is_finite().then_some(raw as i64)
}

mod records;

use records::ClaudeParser;
