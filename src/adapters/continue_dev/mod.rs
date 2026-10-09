//! Adapter: Continue (the VS Code / JetBrains extension and the `cn` CLI) —
//!   `~/.continue/sessions/<session id>.json`, one document per session,
//!   rewritten as the session grows, beside the `sessions.json` index.
//!
//! Source: continuedev/continue `core/index.d.ts` (`Session {sessionId, title,
//! workspaceDirectory, history: ChatHistoryItem[]}`, `ChatHistoryItem
//! {message, contextItems, ...}`, `ChatMessage` = user | assistant | thinking
//! | tool | system with `content: string | MessagePart[]`, an assistant's
//! `toolCalls [{id, function: {name, arguments}}]` and a tool message's
//! `toolCallId`) and `core/util/history.ts` (`getSessionFilePath`:
//! `<global dir>/sessions/<id>.json`).
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::common::{
    as_text, entry, event, existing_root, file_name, files_with_suffix, is_file, read_json,
    text_field, text_of, tool_call, tool_result,
};
use crate::types::{Adapter, ParserCtx, RawEvent, Reading, SessionEntry};
use crate::util::Result;

/// The index Continue keeps beside its sessions; it is not a session.
const INDEX: &str = "sessions.json";

pub struct Continue;

impl Adapter for Continue {
    fn runtime(&self) -> &'static str {
        "continue"
    }

    fn roots(&self, home: &Path) -> Vec<PathBuf> {
        existing_root(home.join(".continue").join("sessions"))
    }

    fn list_sessions(&self, root: &Path) -> Vec<SessionEntry> {
        files_with_suffix(root, ".json")
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
        Reading::Whole(Box::new(move || load(&ctx)))
    }
}

fn session_entry(file: &Path) -> Option<SessionEntry> {
    let name = file_name(file)?;
    if name == INDEX {
        return None;
    }
    Some(entry(
        file.to_path_buf(),
        Some(name.strip_suffix(".json")?.to_string()),
        None,
    ))
}

fn load(ctx: &ParserCtx) -> Result<Vec<RawEvent>> {
    let session = read_json(&ctx.file)?;
    let project = text_field(&session, "workspaceDirectory");
    let mut events = Vec::new();
    for item in session
        .get("history")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(message) = item.get("message") else {
            continue;
        };
        let words = text_of(message.get("content"));
        match message.get("role").and_then(Value::as_str) {
            Some("user") if !words.is_empty() => events.push(event(ctx, None, "user", words)),
            Some("thinking") if !words.is_empty() => {
                events.push(event(ctx, None, "thinking", words))
            }
            Some("assistant") => {
                if !words.is_empty() {
                    events.push(event(ctx, None, "assistant", words));
                }
                for call in message
                    .get("toolCalls")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    let function = call.get("function");
                    events.push(tool_call(
                        ctx,
                        None,
                        function
                            .and_then(|function| function.get("name"))
                            .and_then(Value::as_str),
                        as_text(function.and_then(|function| function.get("arguments"))),
                        call.get("id").and_then(Value::as_str),
                    ));
                }
            }
            Some("tool") => events.push(tool_result(
                ctx,
                None,
                None,
                words,
                message.get("toolCallId").and_then(Value::as_str),
                None,
            )),
            _ => {}
        }
    }
    for event in &mut events {
        event.project = project.clone();
    }
    Ok(events)
}
