//! Adapter: Grok Build (xAI's `grok` CLI) —
//!   `~/.grok/sessions/<encoded cwd>/<session id>/updates.jsonl`, the session's
//!   Agent Client Protocol updates appended one per line, beside
//!   `summary.json` (`{info: {id, cwd}, generated_title?, ...}`).
//!
//! Source: the observed-format registry vshulcz/deja-vu `docs/registry/grok.md`
//! and its fixture (Grok Build 1.0.5–1.0.41), and ACP's `session/update`
//! notification: each line is `{timestamp, params: {update: {sessionUpdate,
//! ...}, _meta: {promptId?, agentTimestampMs?}}}` with
//!   user_message_chunk {content}   -> user
//!   agent_message_chunk {content}  -> assistant
//!   agent_thought_chunk {content}  -> thinking
//!   tool_call {toolCallId, title, rawInput}          -> tool_call
//!   tool_call_update {toolCallId, status, rawOutput | content} once the
//!     status is completed or failed                  -> tool_result
//! Content is `{type: text, text}` or a list of such parts. `timestamp` is in
//! seconds or milliseconds; a value later than now in seconds is milliseconds.
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::common::{
    as_text, entry, event, existing_root, int_field, is_file, iso_millis, iso_seconds, read_json,
    subdirectories, text_field, text_of, tool_call, tool_result,
};
use crate::types::{Adapter, Parser, ParserCtx, RawEvent, Reading, SessionEntry};

const UPDATES: &str = "updates.jsonl";

pub struct Grok;

impl Adapter for Grok {
    fn runtime(&self) -> &'static str {
        "grok"
    }

    fn watch_roots(&self, home: &Path) -> Vec<PathBuf> {
        existing_root(home.join(".grok").join("sessions"))
    }

    fn roots(&self, home: &Path) -> Vec<PathBuf> {
        existing_root(home.join(".grok").join("sessions"))
            .iter()
            .flat_map(|base| subdirectories(base))
            .collect()
    }

    fn list_sessions(&self, root: &Path) -> Vec<SessionEntry> {
        subdirectories(root)
            .iter()
            .filter_map(|session| session_entry(session))
            .collect()
    }

    fn entry_for(&self, path: &Path) -> Option<SessionEntry> {
        let session = path.parent()?;
        let root = session.parent()?;
        if path.file_name()? != UPDATES
            || !self
                .roots(&crate::util::home_dir())
                .iter()
                .any(|known| known.as_path() == root)
        {
            return None;
        }
        session_entry(session)
    }

    fn read(&self, ctx: ParserCtx) -> Reading {
        Reading::Lines(Box::new(GrokParser { ctx }))
    }
}

fn session_entry(session: &Path) -> Option<SessionEntry> {
    let updates = session.join(UPDATES);
    if !is_file(&updates) {
        return None;
    }
    let info = read_json(&session.join("summary.json"))
        .ok()
        .and_then(|summary| summary.get("info").cloned());
    let id = info
        .as_ref()
        .and_then(|info| text_field(info, "id"))
        .or_else(|| Some(session.file_name()?.to_string_lossy().to_string()));
    let project = info.as_ref().and_then(|info| text_field(info, "cwd"));
    Some(entry(updates, id, project))
}

/// The time a line carries.
fn moment(record: &Value) -> Option<String> {
    if let Some(millis) = record
        .get("params")
        .and_then(|params| params.get("_meta"))
        .and_then(|meta| int_field(meta, "agentTimestampMs"))
    {
        return iso_millis(millis);
    }
    let stamp = record.get("timestamp").and_then(Value::as_f64)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs_f64();
    if stamp > now {
        iso_millis(stamp as i64)
    } else {
        iso_seconds(stamp)
    }
}

struct GrokParser {
    ctx: ParserCtx,
}

impl Parser for GrokParser {
    fn on_line(&mut self, line: &str) -> Vec<RawEvent> {
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            return Vec::new();
        };
        let Some(update) = record.get("params").and_then(|params| params.get("update")) else {
            return Vec::new();
        };
        let ctx = &self.ctx;
        let ts = moment(&record);
        let words = text_of(update.get("content")).to_string();
        let words = if words.is_empty() {
            as_text(
                update
                    .get("content")
                    .and_then(|content| content.get("text")),
            )
        } else {
            words
        };
        let id = update.get("toolCallId").and_then(Value::as_str);
        match text_field(update, "sessionUpdate").as_deref() {
            Some("user_message_chunk") if !words.is_empty() => vec![event(ctx, ts, "user", words)],
            Some("agent_message_chunk") if !words.is_empty() => {
                vec![event(ctx, ts, "assistant", words)]
            }
            Some("agent_thought_chunk") if !words.is_empty() => {
                vec![event(ctx, ts, "thinking", words)]
            }
            Some("tool_call") => vec![tool_call(
                ctx,
                ts,
                update.get("title").and_then(Value::as_str),
                as_text(update.get("rawInput")),
                id,
            )],
            Some("tool_call_update") => match text_field(update, "status").as_deref() {
                Some(status @ ("completed" | "failed")) => {
                    let output = match update.get("rawOutput") {
                        Some(raw) => as_text(Some(raw)),
                        None => words,
                    };
                    vec![tool_result(
                        ctx,
                        ts,
                        None,
                        output,
                        id,
                        Some(status == "failed"),
                    )]
                }
                _ => Vec::new(),
            },
            _ => Vec::new(),
        }
    }
}
