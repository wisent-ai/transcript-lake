//! Adapter: Cursor (the editor's Composer and Agent chats) —
//!   `<Cursor user dir>/globalStorage/state.vscdb`, the editor's SQLite
//!   key-value store, table `cursorDiskKV (key, value)`.
//!
//! Cursor publishes no format; this is the layout read from the store on
//! this machine (`sqlite3 -readonly ... .schema` and the keys below), which
//! community readers of Cursor history describe the same way:
//!   `composerData:<composer id>` — one chat: `{composerId, name, createdAt,
//!     lastUpdatedAt, fullConversationHeadersOnly: [{bubbleId, type}], ...}`
//!     (epoch milliseconds);
//!   `bubbleId:<composer id>:<bubble id>` — one message: `{type, text,
//!     thinking?: {text}, toolFormerData?: {name, rawArgs, result, status,
//!     toolCallId}, tokenCount: {inputTokens, outputTokens}, timingInfo?:
//!     {clientRpcSendTime, clientSettleTime}}`.
//! A bubble's `type` is Cursor's message type, numbered as `MESSAGE_TYPES`
//! lists them: unspecified, the person, the model. A chat's messages are
//! read in the order its headers give. A token count Cursor left at zero
//! (it writes one on every bubble before the model answers) is no usage.
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::common::{
    as_text, database_error, database_of, editor_user_dirs, entry, event, int_field, is_file,
    iso_millis, iso_millis_field, open_database, text_field, tool_call, tool_result, usage,
};
use crate::types::{Adapter, ParserCtx, RawEvent, Reading, SessionEntry};
use crate::util::Result;

const DATABASE: &str = "state.vscdb";

/// Who a bubble is from.
#[derive(Clone, Copy, PartialEq)]
enum MessageType {
    Unspecified,
    Person,
    Model,
}

/// Cursor's message types in their numbering: each one's number is its
/// index here.
const MESSAGE_TYPES: &[MessageType] = &[
    MessageType::Unspecified,
    MessageType::Person,
    MessageType::Model,
];

pub struct Cursor;

impl Adapter for Cursor {
    fn runtime(&self) -> &'static str {
        "cursor"
    }

    fn roots(&self, home: &Path) -> Vec<PathBuf> {
        editor_user_dirs(home)
            .into_iter()
            .filter(|user| {
                user.parent()
                    .and_then(Path::file_name)
                    .is_some_and(|editor| editor == "Cursor")
            })
            .map(|user| user.join("globalStorage"))
            .filter(|storage| storage.is_dir())
            .collect()
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

/// Every value under keys starting with `prefix`, by key.
fn values(ctx: &ParserCtx, prefix: &str) -> Result<HashMap<String, Value>> {
    let path = &ctx.file;
    let database = open_database(path)?;
    let mut statement = database
        .prepare("SELECT key, value FROM cursorDiskKV WHERE key LIKE ?1 || '%'")
        .map_err(|error| database_error(path, "its chats", error))?;
    let rows = statement
        .query_map([prefix], |row| {
            Ok((
                row.get::<_, String>("key")?,
                row.get::<_, Option<String>>("value")?,
            ))
        })
        .map_err(|error| database_error(path, "its chats", error))?;
    let mut found = HashMap::new();
    for row in rows {
        let (key, value) = row.map_err(|error| database_error(path, "a chat record", error))?;
        if let Some(value) = value.and_then(|value| serde_json::from_str::<Value>(&value).ok()) {
            found.insert(key, value);
        }
    }
    Ok(found)
}

fn load(ctx: &ParserCtx) -> Result<Vec<RawEvent>> {
    let composers = values(ctx, "composerData:")?;
    let bubbles = values(ctx, "bubbleId:")?;
    let mut ordered: Vec<&Value> = composers.values().collect();
    ordered.sort_by_key(|composer| int_field(composer, "createdAt"));
    let mut events = Vec::new();
    for composer in ordered {
        let Some(id) = text_field(composer, "composerId") else {
            continue;
        };
        let started = iso_millis_field(composer, "createdAt");
        for header in composer
            .get("fullConversationHeadersOnly")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(bubble) = text_field(header, "bubbleId")
                .and_then(|bubble| bubbles.get(&format!("bubbleId:{id}:{bubble}")))
            else {
                continue;
            };
            let mut found = bubble_events(ctx, bubble, started.clone());
            for event in &mut found {
                event.session_id = Some(id.clone());
            }
            events.extend(found);
        }
    }
    Ok(events)
}

fn bubble_events(ctx: &ParserCtx, bubble: &Value, started: Option<String>) -> Vec<RawEvent> {
    let kind = bubble
        .get("type")
        .and_then(Value::as_u64)
        .and_then(|number| usize::try_from(number).ok())
        .and_then(|number| MESSAGE_TYPES.get(number).copied());
    let ts = bubble
        .get("timingInfo")
        .and_then(|timing| timing.get("clientRpcSendTime"))
        .and_then(Value::as_f64)
        .and_then(|millis| iso_millis(millis as i64))
        .or(started);
    let mut events = Vec::new();
    if let Some(thought) = bubble
        .get("thinking")
        .and_then(|thinking| text_field(thinking, "text"))
    {
        events.push(event(ctx, ts.clone(), "thinking", thought));
    }
    if let Some(text) = text_field(bubble, "text") {
        match kind {
            Some(MessageType::Person) => events.push(event(ctx, ts.clone(), "user", text)),
            Some(MessageType::Model) => events.push(event(ctx, ts.clone(), "assistant", text)),
            Some(MessageType::Unspecified) | None => {}
        }
    }
    if let Some(tool) = bubble.get("toolFormerData").filter(|tool| tool.is_object()) {
        let name = tool.get("name").and_then(Value::as_str);
        let id = tool.get("toolCallId").and_then(Value::as_str);
        events.push(tool_call(
            ctx,
            ts.clone(),
            name,
            as_text(tool.get("rawArgs")),
            id,
        ));
        if let Some(result) = tool.get("result").filter(|result| !result.is_null()) {
            let failed = tool
                .get("status")
                .and_then(Value::as_str)
                .map(|status| status == "error");
            events.push(tool_result(
                ctx,
                ts.clone(),
                name,
                as_text(Some(result)),
                id,
                failed,
            ));
        }
    }
    let tokens = bubble.get("tokenCount");
    let count = |key: &str| {
        tokens
            .and_then(|tokens| int_field(tokens, key))
            .filter(|count| count.is_positive())
    };
    if count("inputTokens").is_some() || count("outputTokens").is_some() {
        events.push(usage(
            ctx,
            ts,
            None,
            count("inputTokens"),
            count("outputTokens"),
        ));
    }
    events
}
