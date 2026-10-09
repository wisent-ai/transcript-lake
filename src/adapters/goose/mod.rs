//! Adapter: Goose (Block's agent; the CLI and the desktop app share it) —
//!   `~/.local/share/goose/sessions/sessions.db`, one SQLite database holding
//!   every session.
//!
//! Source: block/goose `crates/goose/src/session/session_manager.rs`
//! (`SESSIONS_FOLDER` "sessions", `DB_NAME` "sessions.db" under
//! `Paths::data_dir()`), tables
//!   sessions (id, name, working_dir, ...),
//!   messages (id, message_id, session_id, role, content_json,
//!     created_timestamp, timestamp, metadata_json),
//! and `crates/goose/src/conversation/message.rs`: `content_json` is a list
//! of `MessageContent` tagged by `type` — text {text}, thinking {thinking},
//! redactedThinking, toolRequest {id, toolCall: {status, value: {name,
//! arguments}}}, toolResponse {id, toolResult: {status, value}}, image,
//! systemNotification {msg}, and the confirmation / summarization kinds.
//! `timestamp` is SQLite's `YYYY-MM-DD HH:MM:SS` in UTC; `created_timestamp`
//! is in seconds or milliseconds depending on the writer, so the text column
//! is the one read.
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use super::common::{
    as_text, database_error, database_of, entry, event, existing_root, is_file, meta,
    open_database, text_field, tool_call, tool_result,
};
use crate::types::{Adapter, ParserCtx, RawEvent, Reading, SessionEntry};
use crate::util::Result;

const DATABASE: &str = "sessions.db";

pub struct Goose;

impl Adapter for Goose {
    fn runtime(&self) -> &'static str {
        "goose"
    }

    fn roots(&self, home: &Path) -> Vec<PathBuf> {
        existing_root(
            home.join(".local")
                .join("share")
                .join("goose")
                .join("sessions"),
        )
    }

    fn list_sessions(&self, root: &Path) -> Vec<SessionEntry> {
        let database = root.join(DATABASE);
        if is_file(&database) {
            vec![entry(database, None, None)]
        } else {
            Vec::new()
        }
    }

    fn entry_for(&self, path: &Path) -> Option<SessionEntry> {
        let database = database_of(path);
        let root = database.parent()?;
        if !self
            .roots(&crate::util::home_dir())
            .iter()
            .any(|known| known.as_path() == root)
        {
            return None;
        }
        self.list_sessions(root)
            .into_iter()
            .find(|session| session.file == database)
    }

    fn read(&self, ctx: ParserCtx) -> Reading {
        Reading::Whole(Box::new(move || load(&ctx)))
    }
}

const MESSAGES: &str = "SELECT messages.role AS role, messages.content_json AS content, \
     messages.timestamp AS stamp, messages.session_id AS session_id, sessions.working_dir AS directory \
     FROM messages JOIN sessions ON sessions.id = messages.session_id \
     ORDER BY messages.session_id, messages.created_timestamp, messages.id";

fn load(ctx: &ParserCtx) -> Result<Vec<RawEvent>> {
    let path = &ctx.file;
    let database = open_database(path)?;
    let mut statement = database
        .prepare(MESSAGES)
        .map_err(|error| database_error(path, "its messages", error))?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>("role")?,
                row.get::<_, String>("content")?,
                row.get::<_, Option<String>>("stamp")?,
                row.get::<_, String>("session_id")?,
                row.get::<_, Option<String>>("directory")?,
            ))
        })
        .map_err(|error| database_error(path, "its messages", error))?;
    let mut events = Vec::new();
    for row in rows {
        let (role, content, stamp, session, directory) =
            row.map_err(|error| database_error(path, "a message", error))?;
        let Ok(content) = serde_json::from_str::<Value>(&content) else {
            continue;
        };
        let ts = stamp.map(|stamp| format!("{}Z", stamp.replacen(' ', "T", 1)));
        let mut found = content_events(ctx, &role, &content, ts);
        for event in &mut found {
            event.session_id = Some(session.clone());
            event.project = directory.clone();
        }
        events.extend(found);
    }
    Ok(events)
}

fn content_events(
    ctx: &ParserCtx,
    role: &str,
    content: &Value,
    ts: Option<String>,
) -> Vec<RawEvent> {
    let speaker = if role == "assistant" {
        "assistant"
    } else {
        "user"
    };
    let mut events = Vec::new();
    for block in content.as_array().into_iter().flatten() {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(text) = text_field(block, "text") {
                    events.push(event(ctx, ts.clone(), speaker, text));
                }
            }
            Some("thinking") => {
                if let Some(thought) = text_field(block, "thinking") {
                    events.push(event(ctx, ts.clone(), "thinking", thought));
                }
            }
            Some("toolRequest") => {
                let call = block.get("toolCall").and_then(|call| call.get("value"));
                events.push(tool_call(
                    ctx,
                    ts.clone(),
                    call.and_then(|call| call.get("name"))
                        .and_then(Value::as_str),
                    as_text(call.and_then(|call| call.get("arguments"))),
                    block.get("id").and_then(Value::as_str),
                ));
            }
            Some("toolResponse") => {
                let result = block.get("toolResult");
                let failed = result
                    .and_then(|result| result.get("status"))
                    .and_then(Value::as_str)
                    .map(|status| status != "success");
                let value = result.and_then(|result| result.get("value"));
                let blocks = match value.and_then(|value| value.get("content")) {
                    Some(inner) => Some(inner),
                    None => value,
                };
                let output = blocks
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|item| text_field(item, "text"))
                    .collect::<Vec<_>>()
                    .join("\n");
                events.push(tool_result(
                    ctx,
                    ts.clone(),
                    None,
                    output,
                    block.get("id").and_then(Value::as_str),
                    failed,
                ));
            }
            Some("systemNotification") => {
                events.push(meta(
                    ctx,
                    ts.clone(),
                    "system_notification",
                    as_text(block.get("msg")),
                    Map::new(),
                ));
            }
            Some(other) => events.push(meta(ctx, ts.clone(), other, String::new(), Map::new())),
            None => {}
        }
    }
    events
}
