//! Adapter: Command Code (`cmd`) —
//!   `~/.commandcode/projects/<encoded cwd>/<session>.jsonl`, beside the
//!   `<session>.checkpoints.jsonl` and `<session>.prompts.jsonl` side streams,
//!   which are not transcripts.
//!
//! Source: the observed-format registry vshulcz/deja-vu
//! `docs/registry/commandcode.md` (command-code 1.73.4–1.77.0). Format 3:
//!   {type: session, version, id, timestamp, cwd}      -> meta; names the
//!                                                       session and project
//!   {type: message, timestamp, message: {role, content: Claude's blocks}}
//!                                                    -> user / assistant /
//!                                                       tool_call / tool_result
//!   {type: compaction, summary, firstKeptEntryId}     -> meta with the summary
//! The older format is one flat `{role, content, timestamp, sessionId}` per line.
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use super::common::{
    anthropic_blocks, as_text, entry, existing_root, file_name, files_with_suffix, is_file, meta,
    subdirectories, text_field,
};
use crate::types::{Adapter, Parser, ParserCtx, RawEvent, Reading, SessionEntry};

/// The side streams beside a transcript, by the suffix Command Code gives them.
const SIDE_STREAMS: &[&str] = &[".checkpoints.jsonl", ".prompts.jsonl"];

pub struct CommandCode;

impl Adapter for CommandCode {
    fn runtime(&self) -> &'static str {
        "commandcode"
    }

    fn watch_roots(&self, home: &Path) -> Vec<PathBuf> {
        existing_root(home.join(".commandcode").join("projects"))
    }

    fn roots(&self, home: &Path) -> Vec<PathBuf> {
        existing_root(home.join(".commandcode").join("projects"))
            .iter()
            .flat_map(|base| subdirectories(base))
            .collect()
    }

    fn list_sessions(&self, root: &Path) -> Vec<SessionEntry> {
        files_with_suffix(root, ".jsonl")
            .iter()
            .filter_map(|file| session_entry(file))
            .collect()
    }

    fn entry_for(&self, path: &Path) -> Option<SessionEntry> {
        let root = path.parent()?;
        if !self
            .roots(&crate::util::home_dir())
            .iter()
            .any(|known| known.as_path() == root)
            || !is_file(path)
        {
            return None;
        }
        session_entry(path)
    }

    fn read(&self, ctx: ParserCtx) -> Reading {
        Reading::Lines(Box::new(CommandCodeParser { ctx }))
    }
}

fn session_entry(file: &Path) -> Option<SessionEntry> {
    let name = file_name(file)?;
    if SIDE_STREAMS.iter().any(|suffix| name.ends_with(suffix)) {
        return None;
    }
    Some(entry(
        file.to_path_buf(),
        Some(name.strip_suffix(".jsonl")?.to_string()),
        None,
    ))
}

struct CommandCodeParser {
    ctx: ParserCtx,
}

impl Parser for CommandCodeParser {
    fn on_line(&mut self, line: &str) -> Vec<RawEvent> {
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            return Vec::new();
        };
        let ts = text_field(&record, "timestamp");
        match text_field(&record, "type").as_deref() {
            Some("session") => {
                if let Some(id) = text_field(&record, "id") {
                    self.ctx.session_id = Some(id);
                }
                if let Some(cwd) = text_field(&record, "cwd") {
                    self.ctx.project = Some(cwd);
                }
                vec![meta(&self.ctx, ts, "session", String::new(), Map::new())]
            }
            Some("message") => {
                let Some(message) = record.get("message") else {
                    return Vec::new();
                };
                match text_field(message, "role") {
                    Some(role) => anthropic_blocks(&self.ctx, &role, message.get("content"), ts),
                    None => Vec::new(),
                }
            }
            Some("compaction") => vec![meta(
                &self.ctx,
                ts,
                "compaction",
                as_text(record.get("summary")),
                Map::new(),
            )],
            Some(other) => vec![meta(&self.ctx, ts, other, String::new(), Map::new())],
            None => match text_field(&record, "role") {
                Some(role) => anthropic_blocks(&self.ctx, &role, record.get("content"), ts),
                None => Vec::new(),
            },
        }
    }
}
