//! Adapter: Gemini CLI —
//!   `~/.gemini/tmp/<project-id>/chats/session-<stamp>-<id>.jsonl` (and the
//!   older whole-document `session-*.json`), with the project's absolute path
//!   in `~/.gemini/tmp/<project-id>/.project_root`.
//!
//! Source: google-gemini/gemini-cli `packages/core/src/services/
//! chatRecordingService.ts`, `chatRecordingTypes.ts` and
//! `config/projectRegistry.ts`. A JSONL session holds, one per line:
//!   `{sessionId, projectHash, startTime, directories?, kind?}` metadata,
//!   `{$set: {...}}` metadata updates (a legacy one carries `messages`),
//!   message records `{id, timestamp, type, content, ...}` where type is
//!     user | gemini | info | error | warning, and a gemini message carries
//!     `thoughts [{subject, description, timestamp}]`, `toolCalls [{id, name,
//!     args, result, status, timestamp}]`, `tokens {input, output, cached,
//!     thoughts, tool, total}` and `model`,
//!   `{$patch: {id?, content?, toolCalls? [{id, result}], updates?, removeIds?,
//!     orderIds?}}` that rewrite earlier messages, and `{$rewindTo: id}`.
//! Because later lines rewrite earlier messages, a session is read whole and
//! every message is replayed with its patches applied. A rewind or removal
//! hides messages from the harness's own history, but what was said stays in
//! the archive, so neither deletes an event here.
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use super::parts::{part_events, parts};
use crate::adapters::common::{
    as_text, entry, event, file_name, files_with_suffix, is_file, meta, read_json, read_json_lines,
    subdirectories, text_field, tool_call, tool_result, usage,
};
use crate::types::{Adapter, ParserCtx, RawEvent, Reading, SessionEntry};
use crate::util::Result;

const PREFIX: &str = "session-";
const CHATS: &str = "chats";
const PROJECT_ROOT: &str = ".project_root";

pub struct Gemini;

impl Adapter for Gemini {
    fn runtime(&self) -> &'static str {
        "gemini"
    }

    fn roots(&self, home: &Path) -> Vec<PathBuf> {
        chat_roots(&home.join(".gemini").join("tmp"))
    }

    fn watch_roots(&self, home: &Path) -> Vec<PathBuf> {
        crate::adapters::common::existing_root(home.join(".gemini").join("tmp"))
    }

    fn list_sessions(&self, root: &Path) -> Vec<SessionEntry> {
        list_chats(root)
    }

    fn entry_for(&self, path: &Path) -> Option<SessionEntry> {
        chat_entry(&self.roots(&crate::util::home_dir()), path)
    }

    fn read(&self, ctx: ParserCtx) -> Reading {
        Reading::Whole(Box::new(move || load(&ctx)))
    }
}

/// The `chats` directory of every project under a Gemini-family `tmp` root.
pub(super) fn chat_roots(tmp: &Path) -> Vec<PathBuf> {
    subdirectories(tmp)
        .into_iter()
        .map(|project| project.join(CHATS))
        .filter(|chats| chats.is_dir())
        .collect()
}

/// Every session file in one `chats` directory.
pub(super) fn list_chats(root: &Path) -> Vec<SessionEntry> {
    let mut sessions: Vec<SessionEntry> = Vec::new();
    for suffix in [".jsonl", ".json"] {
        for file in files_with_suffix(root, suffix) {
            if let Some(session) = session_entry(&file) {
                sessions.push(session);
            }
        }
    }
    sessions
}

/// The entry for a path a notification named, when it is a session file in
/// one of `roots`.
pub(super) fn chat_entry(roots: &[PathBuf], path: &Path) -> Option<SessionEntry> {
    let chats = path.parent()?;
    if !roots.iter().any(|root| root.as_path() == chats) || !is_file(path) {
        return None;
    }
    session_entry(path)
}

fn session_entry(file: &Path) -> Option<SessionEntry> {
    let name = file_name(file)?;
    let stem = name
        .strip_suffix(".jsonl")
        .or_else(|| name.strip_suffix(".json"))?;
    let id = stem.strip_prefix(PREFIX)?;
    let project = std::fs::read_to_string(file.parent()?.parent()?.join(PROJECT_ROOT))
        .ok()
        .map(|root| root.trim().to_string())
        .filter(|root| !root.is_empty());
    Some(entry(file.to_path_buf(), Some(id.to_string()), project))
}

/// Every message of a session file, in order, with its patches applied.
pub(super) fn messages(file: &Path) -> Result<(Option<String>, Vec<Value>)> {
    if file
        .extension()
        .is_some_and(|extension| extension == "json")
    {
        let document = read_json(file)?;
        let messages = document
            .get("messages")
            .and_then(Value::as_array)
            .cloned()
            .into_iter()
            .flatten()
            .collect();
        return Ok((text_field(&document, "sessionId"), messages));
    }
    let mut session = None;
    let mut order: Vec<String> = Vec::new();
    let mut by_id: HashMap<String, Value> = HashMap::new();
    for record in read_json_lines(file)? {
        if let Some(set) = record.get("$set") {
            for message in set
                .get("messages")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                put(message, &mut order, &mut by_id);
            }
        } else if let Some(patch) = record.get("$patch") {
            apply_patch(patch, &mut by_id);
            for update in patch
                .get("updates")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                apply_patch(update, &mut by_id);
            }
        } else if record.get("$rewindTo").is_some() {
            continue;
        } else if record.get("projectHash").is_some() {
            session = text_field(&record, "sessionId");
        } else {
            put(&record, &mut order, &mut by_id);
        }
    }
    Ok((
        session,
        order
            .into_iter()
            .filter_map(|id| by_id.remove(&id))
            .collect(),
    ))
}

/// Record `message` under its id, keeping the place it first had.
fn put(message: &Value, order: &mut Vec<String>, by_id: &mut HashMap<String, Value>) {
    if let Some(id) = text_field(message, "id") {
        if !by_id.contains_key(&id) {
            order.push(id.clone());
        }
        by_id.insert(id, message.clone());
    }
}

/// Apply one message patch: new content, and results for its tool calls.
fn apply_patch(patch: &Value, by_id: &mut HashMap<String, Value>) {
    let Some(id) = text_field(patch, "id") else {
        return;
    };
    let Some(message) = by_id.get_mut(&id) else {
        return;
    };
    if let Some(content) = patch.get("content") {
        message["content"] = content.clone();
    }
    for update in patch
        .get("toolCalls")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(id) = update.get("id") else { continue };
        let Some(calls) = message.get_mut("toolCalls").and_then(Value::as_array_mut) else {
            continue;
        };
        for call in calls.iter_mut().filter(|call| call.get("id") == Some(id)) {
            if let Some(result) = update.get("result") {
                call["result"] = result.clone();
            }
        }
    }
}

fn load(ctx: &ParserCtx) -> Result<Vec<RawEvent>> {
    let (session, messages) = messages(&ctx.file)?;
    let mut events = Vec::new();
    for message in &messages {
        events.extend(message_events(ctx, message));
    }
    if let Some(session) = session {
        for event in &mut events {
            event.session_id = Some(session.clone());
        }
    }
    Ok(events)
}

/// The events one Gemini CLI message record holds.
pub(super) fn message_events(ctx: &ParserCtx, message: &Value) -> Vec<RawEvent> {
    let ts = text_field(message, "timestamp");
    let content = parts(message.get("content"));
    match message.get("type").and_then(Value::as_str) {
        Some("user") => part_events(ctx, "user", &content, ts),
        Some("gemini") => {
            let mut events = Vec::new();
            for thought in message
                .get("thoughts")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let said = [
                    text_field(thought, "subject"),
                    text_field(thought, "description"),
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(": ");
                if !said.is_empty() {
                    events.push(event(
                        ctx,
                        text_field(thought, "timestamp").or_else(|| ts.clone()),
                        "thinking",
                        said,
                    ));
                }
            }
            events.extend(part_events(ctx, "assistant", &content, ts.clone()));
            for call in message
                .get("toolCalls")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let at = text_field(call, "timestamp").or_else(|| ts.clone());
                let id = call.get("id").and_then(Value::as_str);
                let name = call.get("name").and_then(Value::as_str);
                events.push(tool_call(
                    ctx,
                    at.clone(),
                    name,
                    as_text(call.get("args")),
                    id,
                ));
                if let Some(result) = call.get("result").filter(|result| !result.is_null()) {
                    let failed = call
                        .get("status")
                        .and_then(Value::as_str)
                        .map(|status| status == "error");
                    let output = parts(Some(result))
                        .iter()
                        .map(|part| match part.get("functionResponse") {
                            Some(response) => as_text(response.get("response")),
                            None => as_text(part.get("text")),
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    events.push(tool_result(ctx, at, name, output, id, failed));
                }
            }
            if let Some(tokens) = message.get("tokens").filter(|tokens| tokens.is_object()) {
                let count = |key: &str| tokens.get(key).and_then(Value::as_i64);
                events.push(usage(
                    ctx,
                    ts,
                    text_field(message, "model"),
                    count("input"),
                    count("output"),
                ));
            }
            events
        }
        Some(other) => {
            let words = content
                .iter()
                .map(|part| as_text(part.get("text")))
                .collect::<Vec<_>>()
                .join("\n");
            vec![meta(ctx, ts, other, words, Map::new())]
        }
        None => Vec::new(),
    }
}
