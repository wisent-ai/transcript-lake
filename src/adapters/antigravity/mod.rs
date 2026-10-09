//! Adapter: Google Antigravity (the IDE and its `agy` CLI) —
//!   `~/.gemini/antigravity*/brain/<session id>/.system_generated/logs/
//!   transcript.jsonl`, appended one step per line: `{source, created_at
//!   (RFC 3339), content, type?, status?, step_index?, tool_calls?}`.
//!
//! Source: the observed-format registry vshulcz/deja-vu
//! `docs/registry/antigravity.md` and its fixture (agy 1.1.7–1.3.1):
//!   source USER_EXPLICIT          -> user, the `<USER_REQUEST>` wrapper and its
//!                                    `<ADDITIONAL_METADATA>` block taken off
//!   source MODEL, type PLANNER_RESPONSE or none -> assistant, with each of its
//!                                    `tool_calls [{name, args}]` a tool_call
//!   source MODEL, any other type (RUN_COMMAND, VIEW_FILE, CODE_ACTION, ...)
//!                                 -> tool_result named by the type
//!   source SYSTEM, type CHECKPOINT -> meta carrying the compaction summary
//!   any other source              -> nothing
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use super::common::{
    as_text, entry, event, is_file, meta, read_dirents, subdirectories, text_field, tool_call,
    tool_result,
};
use crate::types::{Adapter, Parser, ParserCtx, RawEvent, Reading, SessionEntry};

/// Where a session directory keeps its transcript.
const TRANSCRIPT: &[&str] = &[".system_generated", "logs", "transcript.jsonl"];

pub struct Antigravity;

impl Adapter for Antigravity {
    fn runtime(&self) -> &'static str {
        "antigravity"
    }

    fn roots(&self, home: &Path) -> Vec<PathBuf> {
        let gemini = home.join(".gemini");
        read_dirents(&gemini)
            .into_iter()
            .filter(|(name, kind)| kind.is_dir() && name.starts_with("antigravity"))
            .map(|(name, _)| gemini.join(name).join("brain"))
            .filter(|brain| brain.is_dir())
            .collect()
    }

    fn list_sessions(&self, root: &Path) -> Vec<SessionEntry> {
        subdirectories(root)
            .iter()
            .filter_map(|session| session_entry(session))
            .collect()
    }

    fn entry_for(&self, path: &Path) -> Option<SessionEntry> {
        let session = path.parent()?.parent()?.parent()?;
        let root = session.parent()?;
        if !self
            .roots(&crate::util::home_dir())
            .iter()
            .any(|known| known.as_path() == root)
        {
            return None;
        }
        session_entry(session).filter(|found| found.file == path)
    }

    fn read(&self, ctx: ParserCtx) -> Reading {
        Reading::Lines(Box::new(AntigravityParser { ctx }))
    }
}

fn session_entry(session: &Path) -> Option<SessionEntry> {
    let file = TRANSCRIPT
        .iter()
        .fold(session.to_path_buf(), |path, part| path.join(part));
    if !is_file(&file) {
        return None;
    }
    Some(entry(
        file,
        Some(session.file_name()?.to_string_lossy().to_string()),
        None,
    ))
}

/// The person's words inside `<USER_REQUEST>`, without the metadata block.
fn request_text(content: &str) -> String {
    let inner = content
        .strip_prefix("<USER_REQUEST>")
        .and_then(|rest| rest.strip_suffix("</USER_REQUEST>"));
    let words = match inner {
        Some(inner) => inner,
        None => content,
    };
    match words.find("<ADDITIONAL_METADATA>") {
        Some(at) => words[..at].trim().to_string(),
        None => words.trim().to_string(),
    }
}

struct AntigravityParser {
    ctx: ParserCtx,
}

impl Parser for AntigravityParser {
    fn on_line(&mut self, line: &str) -> Vec<RawEvent> {
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            return Vec::new();
        };
        let ctx = &self.ctx;
        let ts = text_field(&record, "created_at");
        let content = as_text(record.get("content"));
        let kind = text_field(&record, "type");
        match (text_field(&record, "source").as_deref(), kind.as_deref()) {
            (Some("USER_EXPLICIT"), _) => vec![event(ctx, ts, "user", request_text(&content))],
            (Some("MODEL"), None) | (Some("MODEL"), Some("PLANNER_RESPONSE")) => {
                let mut events = Vec::new();
                if !content.is_empty() {
                    events.push(event(ctx, ts.clone(), "assistant", content));
                }
                for call in record
                    .get("tool_calls")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    let name = call.get("name").and_then(Value::as_str);
                    events.push(tool_call(
                        ctx,
                        ts.clone(),
                        name,
                        as_text(call.get("args")),
                        None,
                    ));
                }
                events
            }
            (Some("MODEL"), Some(step)) => {
                let failed = text_field(&record, "status").map(|status| status != "DONE");
                vec![tool_result(ctx, ts, Some(step), content, None, failed)]
            }
            (Some("SYSTEM"), Some("CHECKPOINT")) => {
                vec![meta(ctx, ts, "checkpoint", content, Map::new())]
            }
            _ => Vec::new(),
        }
    }
}
