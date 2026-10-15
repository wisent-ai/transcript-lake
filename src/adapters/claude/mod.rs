//! Adapter: Claude Code transcripts — `~/.claude/projects/<encoded-cwd>/<sessionId>.jsonl` —
//! and every harness that writes Claude Code's transcripts elsewhere: Cherry
//! Studio runs Claude Code with its config dir under the app's data,
//! `~/Library/Application Support/CherryStudio/Data/Agents/.claude/projects`
//! (`~/.config/CherryStudio/...` on Linux; the observed-format registry
//! vshulcz/deja-vu `docs/registry/cherrystudio.md`).
//!
//! Interface: `runtime`, `roots(home)`, `list_sessions(root)`, `entry_for(path)`, `read(ctx)`.
//! Adapters emit UNMASKED text (the stream masks) and never do IO in `on_line`.
//! Contract: malformed lines are tolerated silently (no events).
//! Record types user | assistant | system | summary carry
//! messages; permission-mode, file-history-snapshot, attachment, ai-title, last-prompt,
//! queue-operation, progress and mode records are bookkeeping noise and are dropped.
use std::path::{Path, PathBuf};

use super::common::{decode_dashed_path, dirent_type, read_dirents};
use crate::types::{Adapter, ParserCtx, Reading, SessionEntry};

/// One harness that writes Claude Code's transcripts: its runtime id and the
/// `projects` directories it may keep them in, under the home.
pub struct ClaudeStore {
    runtime: &'static str,
    projects: &'static [&'static [&'static str]],
}

/// Claude Code.
pub const CLAUDE: ClaudeStore = ClaudeStore {
    runtime: "claude",
    projects: &[&[".claude", "projects"]],
};
/// Cherry Studio's agents.
pub const CHERRY_STUDIO: ClaudeStore = ClaudeStore {
    runtime: "cherrystudio",
    projects: &[
        &[
            "Library",
            "Application Support",
            "CherryStudio",
            "Data",
            "Agents",
            ".claude",
            "projects",
        ],
        &[
            ".config",
            "CherryStudio",
            "Data",
            "Agents",
            ".claude",
            "projects",
        ],
    ],
};

impl Adapter for ClaudeStore {
    fn runtime(&self) -> &'static str {
        self.runtime
    }

    fn roots(&self, home: &Path) -> Vec<PathBuf> {
        self.projects
            .iter()
            .map(|parts| {
                parts
                    .iter()
                    .fold(home.to_path_buf(), |path, part| path.join(part))
            })
            .filter(|dir| dir.exists())
            .collect()
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

    fn read(&self, ctx: ParserCtx) -> Reading {
        Reading::Lines(Box::new(ClaudeParser::new(ctx)))
    }
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
            .and_then(|dir| decode_dashed_path(&dir.to_string_lossy())),
    })
}

mod records;

use records::ClaudeParser;
