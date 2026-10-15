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
//! every message is replayed with its patches applied (`replay`).
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use super::parts::{part_events, parts};
use super::replay::messages;
use crate::adapters::common::{
    as_text, entry, event, file_name, files_with_suffix, is_file, meta, sidecar_text,
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
    let project = sidecar_text(&file.parent()?.parent()?.join(PROJECT_ROOT))
        .map(|root| root.trim().to_string())
        .filter(|root| !root.is_empty());
    Some(entry(file.to_path_buf(), Some(id.to_string()), project))
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
