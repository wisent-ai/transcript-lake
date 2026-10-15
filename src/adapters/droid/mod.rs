//! Adapter for Factory Droid session transcripts.
//!
//! Source layout: `HOME/.factory/sessions/<uuid>.jsonl` (legacy, flat) and
//! `HOME/.factory/sessions/<encoded-cwd>/<uuid>.jsonl`, each with an optional
//! `<uuid>.settings.json` sidecar (multi-line JSON: providerLock, tokenUsage).
//! Supported record types:
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
use std::path::{Path, PathBuf};

use crate::adapters::common::{decode_dashed_path, dirent_type, read_dirents};
use crate::types::{Adapter, ParserCtx, Reading, SessionEntry};

const JSONL_EXT: &str = ".jsonl";
const SETTINGS_EXT: &str = ".settings.json";

pub struct Droid;

impl Adapter for Droid {
    fn runtime(&self) -> &'static str {
        "droid"
    }

    fn roots(&self, home: &Path) -> Vec<PathBuf> {
        let base = home.join(".factory").join("sessions");
        // No sessions directory is no root at all; one reached through a
        // symlink is listed, as reading it always was, and one that cannot be
        // examined is said with its cause.
        match std::fs::metadata(&base) {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => return Vec::new(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
            Err(error) => {
                eprintln!("droid sessions: cannot examine {}: {error}", base.display());
                return Vec::new();
            }
        }
        let mut dirs = vec![base.clone()];
        for (name, file_type) in read_dirents(&base) {
            if file_type.is_dir() {
                dirs.push(base.join(name));
            }
        }
        dirs
    }

    fn list_sessions(&self, root: &Path) -> Vec<SessionEntry> {
        let mut sessions = Vec::new();
        for (name, file_type) in read_dirents(root) {
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

    fn read(&self, ctx: ParserCtx) -> Reading {
        if ctx.file.to_string_lossy().ends_with(SETTINGS_EXT) {
            return Reading::Lines(Box::new(SettingsParser::new(ctx)));
        }
        Reading::Lines(Box::new(TranscriptParser::new(ctx)))
    }
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
    let session_id = name.strip_suffix(extension)?;
    Some(SessionEntry {
        file: root.join(name),
        session_id: Some(session_id.to_string()),
        project: root
            .file_name()
            .and_then(|dir| decode_dashed_path(&dir.to_string_lossy())),
    })
}

mod settings;
mod transcript;

use settings::SettingsParser;
use transcript::TranscriptParser;
