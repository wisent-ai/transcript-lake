//! Adapter: OpenHands (the CLI, on the Software Agent SDK) —
//!   `~/.openhands/conversations/<conversation id>/events/
//!   event-<index>-<event id>.json`, one JSON file per event, beside the
//!   conversation's `base_state.json`.
//!
//! Source: OpenHands docs `openhands/usage/cli/resume` (conversations in
//! `~/.openhands/conversations`), OpenHands/software-agent-sdk
//! `openhands/sdk/conversation/persistence_const.py` (`events/`,
//! `event-{idx:05d}-{event_id}.json`) and `openhands/sdk/event/` (every event
//! has `id`, `timestamp`, `source` user | agent | environment):
//!   MessageEvent {llm_message: {role, content: [{type: text, text}],
//!     reasoning_content?}} — the person's words when `source` is user;
//!   ActionEvent {thought: [{text}], reasoning_content?, tool_name,
//!     tool_call_id, tool_call: {arguments}} — a tool call;
//!   ObservationEvent {tool_name, tool_call_id, observation: {content}},
//!   UserRejectObservation {rejection_reason}, AgentErrorEvent {error} —
//!     tool results;
//!   SystemPromptEvent, Condensation, ConversationStateUpdateEvent, ... —
//!     metadata.
//! Events are told apart by the fields each carries, as listed above. The
//! session is the conversation's `events` directory, read whole whenever a
//! file is added to it.
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use super::common::{
    as_text, entry, event, existing_root, files_with_suffix, meta, read_json, subdirectories,
    text_field, text_of, tool_call, tool_result,
};
use crate::types::{Adapter, ParserCtx, RawEvent, Reading, SessionEntry};
use crate::util::Result;

const EVENTS: &str = "events";

pub struct OpenHands;

impl Adapter for OpenHands {
    fn runtime(&self) -> &'static str {
        "openhands"
    }

    fn roots(&self, home: &Path) -> Vec<PathBuf> {
        existing_root(home.join(".openhands").join("conversations"))
    }

    fn list_sessions(&self, root: &Path) -> Vec<SessionEntry> {
        subdirectories(root)
            .iter()
            .filter_map(|conversation| session_entry(conversation))
            .collect()
    }

    fn entry_for(&self, path: &Path) -> Option<SessionEntry> {
        let events = if path.file_name()? == EVENTS {
            path
        } else {
            path.parent()?
        };
        let conversation = events.parent()?;
        if !self
            .roots(&crate::util::home_dir())
            .iter()
            .any(|root| conversation.parent() == Some(root.as_path()))
        {
            return None;
        }
        session_entry(conversation)
    }

    fn read(&self, ctx: ParserCtx) -> Reading {
        Reading::Whole(Box::new(move || load(&ctx)))
    }
}

fn session_entry(conversation: &Path) -> Option<SessionEntry> {
    let events = conversation.join(EVENTS);
    if !events.is_dir() {
        return None;
    }
    Some(entry(
        events,
        Some(conversation.file_name()?.to_string_lossy().to_string()),
        None,
    ))
}

fn load(ctx: &ParserCtx) -> Result<Vec<RawEvent>> {
    let mut events = Vec::new();
    for file in files_with_suffix(&ctx.file, ".json") {
        events.extend(event_of(ctx, &read_json(&file)?));
    }
    Ok(events)
}

fn event_of(ctx: &ParserCtx, record: &Value) -> Vec<RawEvent> {
    let ts = text_field(record, "timestamp");
    let name = record.get("tool_name").and_then(Value::as_str);
    let id = record.get("tool_call_id").and_then(Value::as_str);
    if let Some(message) = record.get("llm_message") {
        let mut events = Vec::new();
        if let Some(reasoning) = text_field(message, "reasoning_content") {
            events.push(event(ctx, ts.clone(), "thinking", reasoning));
        }
        let words = text_of(message.get("content"));
        if !words.is_empty() {
            let speaker = match text_field(record, "source").as_deref() {
                Some("user") => "user",
                Some("agent") => "assistant",
                _ => return vec![meta(ctx, ts, "environment_message", words, Map::new())],
            };
            events.push(event(ctx, ts, speaker, words));
        }
        return events;
    }
    if let Some(call) = record.get("tool_call") {
        let mut events = Vec::new();
        if let Some(reasoning) = text_field(record, "reasoning_content") {
            events.push(event(ctx, ts.clone(), "thinking", reasoning));
        }
        let thought = text_of(record.get("thought"));
        if !thought.is_empty() {
            events.push(event(ctx, ts.clone(), "assistant", thought));
        }
        events.push(tool_call(ctx, ts, name, as_text(call.get("arguments")), id));
        return events;
    }
    if let Some(observation) = record.get("observation") {
        return vec![tool_result(
            ctx,
            ts,
            name,
            text_of(observation.get("content")),
            id,
            None,
        )];
    }
    if let Some(reason) = text_field(record, "rejection_reason") {
        return vec![tool_result(ctx, ts, name, reason, id, Some(true))];
    }
    if let (Some(error), Some(_)) = (text_field(record, "error"), id) {
        return vec![tool_result(ctx, ts, name, error, id, Some(true))];
    }
    let mut fields = Map::new();
    if let Some(kind) = record.get("kind") {
        fields.insert("event_kind".into(), kind.clone());
    }
    vec![meta(ctx, ts, "openhands_event", String::new(), fields)]
}
