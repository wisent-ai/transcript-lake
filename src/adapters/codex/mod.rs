//! Adapter: Codex CLI rollouts — `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl` — and
//! TRAE CLI, which writes Codex rollouts under `~/.trae/cli/sessions/` (the
//! observed-format registry vshulcz/deja-vu `docs/registry/trae.md`).
//!
//! Interface: `runtime`, `roots(home)`, `list_sessions(root)`, `entry_for(path)`, `read(ctx)`.
//! Adapters emit UNMASKED text (the stream masks) and never do IO in `on_line`;
//! malformed lines are tolerated silently. Envelope per line:
//! `{ timestamp, type, payload }`. Verified on live files, old and current CLI
//! versions: content is duplicated between the response_item stream and event_msg
//! (user_message / agent_message / agent_reasoning). To avoid double counting we take
//! user turns from event_msg/user_message (response_item role=user also carries
//! injected environment context) and everything else from response_item records.
use std::path::{Path, PathBuf};

use regex::Regex;

use super::common::{dirent_type, read_dirents, subdirectories};
use crate::types::{Adapter, ParserCtx, Reading, SessionEntry};

/// Filenames look like `rollout-<ISO-stamp>-<uuid>.jsonl`; the uuid is the session id.
static UUID_RE: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
    Regex::new("(?i)[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}").expect("uuid pattern")
});

/// One harness that writes Codex rollouts: its runtime id and the sessions
/// directory under the home.
pub struct CodexStore {
    runtime: &'static str,
    sessions: &'static [&'static str],
}

/// Codex CLI.
pub const CODEX: CodexStore = CodexStore {
    runtime: "codex",
    sessions: &[".codex", "sessions"],
};
/// TRAE CLI.
pub const TRAE: CodexStore = CodexStore {
    runtime: "trae",
    sessions: &[".trae", "cli", "sessions"],
};

impl Adapter for CodexStore {
    fn runtime(&self) -> &'static str {
        self.runtime
    }

    fn roots(&self, home: &Path) -> Vec<PathBuf> {
        let dir = self
            .sessions
            .iter()
            .fold(home.to_path_buf(), |path, part| path.join(part));
        if dir.exists() {
            return vec![dir];
        }
        Vec::new()
    }

    fn list_sessions(&self, root: &Path) -> Vec<SessionEntry> {
        let mut sessions = Vec::new();
        for year in subdirectories(root) {
            for month in subdirectories(&year) {
                for day in subdirectories(&month) {
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

    fn read(&self, ctx: ParserCtx) -> Reading {
        Reading::Lines(Box::new(CodexParser::new(ctx)))
    }
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

/// The session id a rollout's file name carries: the UUID in its stem, or the
/// whole stem when it holds none. The stem is the name without `.jsonl`, or
/// the whole name when it has another ending.
fn session_id_from_name(name: &str) -> String {
    let stem = match name.strip_suffix(".jsonl") {
        Some(stem) => stem,
        None => name,
    };
    match UUID_RE.find(stem) {
        Some(found) => found.as_str().to_string(),
        None => stem.to_string(),
    }
}

mod records;

use records::CodexParser;
