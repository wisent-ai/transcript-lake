//! Adapter: Cursor CLI (`cursor-agent`) —
//!   `~/.cursor/projects/<encoded path>/agent-transcripts/**/<chat id>.jsonl`,
//!   appended one record per line: `{role, message: {content: Anthropic's
//!   string-or-blocks}}`, with control records such as `{type: turn_ended}`.
//!
//! Source: the observed-format registry vshulcz/deja-vu
//! `docs/registry/cursor.md` and its fixture (cursor-agent 2026.09.02). The
//! records carry no timestamp field; a user turn's text opens with
//! `<timestamp>...</timestamp>`, a local time with its offset, kept in the
//! text as written. Tool calls are Anthropic `tool_use` blocks with Cursor's
//! own names (`Shell`, `Write`, `StrReplace`).
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::common::{
    anthropic_blocks, entry, existing_root, file_name, files_with_suffix, is_file, subdirectories,
    text_field,
};
use crate::types::{Adapter, Parser, ParserCtx, RawEvent, Reading, SessionEntry};

const TRANSCRIPTS: &str = "agent-transcripts";

pub struct CursorAgent;

impl Adapter for CursorAgent {
    fn runtime(&self) -> &'static str {
        "cursor-agent"
    }

    fn watch_roots(&self, home: &Path) -> Vec<PathBuf> {
        existing_root(home.join(".cursor").join("projects"))
    }

    fn roots(&self, home: &Path) -> Vec<PathBuf> {
        existing_root(home.join(".cursor").join("projects"))
            .iter()
            .flat_map(|base| subdirectories(base))
            .map(|project| project.join(TRANSCRIPTS))
            .filter(|transcripts| transcripts.is_dir())
            .collect()
    }

    fn list_sessions(&self, root: &Path) -> Vec<SessionEntry> {
        let mut files = files_with_suffix(root, ".jsonl");
        for nested in subdirectories(root) {
            files.extend(files_with_suffix(&nested, ".jsonl"));
        }
        files
            .iter()
            .filter_map(|file| session_entry(file))
            .collect()
    }

    fn entry_for(&self, path: &Path) -> Option<SessionEntry> {
        let roots = self.roots(&crate::util::home_dir());
        if !roots.iter().any(|root| path.starts_with(root)) || !is_file(path) {
            return None;
        }
        session_entry(path)
    }

    fn read(&self, ctx: ParserCtx) -> Reading {
        Reading::Lines(Box::new(CursorAgentParser { ctx }))
    }
}

fn session_entry(file: &Path) -> Option<SessionEntry> {
    Some(entry(
        file.to_path_buf(),
        Some(file_name(file)?.strip_suffix(".jsonl")?.to_string()),
        None,
    ))
}

struct CursorAgentParser {
    ctx: ParserCtx,
}

impl Parser for CursorAgentParser {
    fn on_line(&mut self, line: &str) -> Vec<RawEvent> {
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            return Vec::new();
        };
        match (text_field(&record, "role"), record.get("message")) {
            (Some(role), Some(message)) => {
                anthropic_blocks(&self.ctx, &role, message.get("content"), None)
            }
            _ => Vec::new(),
        }
    }
}
