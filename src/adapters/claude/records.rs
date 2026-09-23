//! Turning one recorded line into events: the parser that carries the session
//! and project it has seen so far, and what a user record contributes -
//! including a tool result and the persisted output it may only reference.

use serde_json::{Map, Value};

use crate::types::{Parser, ParserCtx, RawEvent};

use super::{cap, text_field};

pub(super) struct ClaudeParser {
    session_id: Option<String>,
    project: Option<String>,
    last_ts: Option<String>,
}

impl ClaudeParser {
    /// A session id or project the caller already knows; an empty string is
    /// no answer at all and is treated as unknown, exactly as the driver did.
    pub(super) fn new(ctx: ParserCtx) -> Self {
        Self {
            session_id: ctx.session_id.filter(|value| !value.is_empty()),
            project: ctx.project.filter(|value| !value.is_empty()),
            last_ts: None,
        }
    }
}

impl Parser for ClaudeParser {
    fn on_line(&mut self, line: &str) -> Vec<RawEvent> {
        if line.trim().is_empty() {
            return Vec::new();
        }
        let Ok(rec) = serde_json::from_str::<Value>(line) else {
            return Vec::new();
        };
        if !rec.is_object() {
            return Vec::new();
        }
        self.map_record(&rec)
    }
}

impl ClaudeParser {
    fn map_record(&mut self, rec: &Value) -> Vec<RawEvent> {
        if let Some(session_id) = text_field(rec, "sessionId") {
            self.session_id = Some(session_id);
        }
        if let Some(cwd) = text_field(rec, "cwd") {
            self.project = Some(cwd);
        }
        if let Some(timestamp) = text_field(rec, "timestamp") {
            self.last_ts = Some(timestamp);
        }
        let Some(ts) = self.last_ts.clone() else {
            return Vec::new();
        };
        let session_id = self.session_id.clone();
        let project = self.project.clone();
        let make = move |event_type: &str, text: &str| RawEvent {
            ts: Some(ts.clone()),
            session_id: session_id.clone(),
            project: project.clone(),
            event_type: event_type.to_string(),
            text: cap(text),
            ..RawEvent::default()
        };
        match rec.get("type").and_then(Value::as_str) {
            Some("user") => map_user(rec, &make),
            Some("assistant") => map_assistant(rec, &make),
            Some("system") => {
                let text = text_field(rec, "content")
                    .or_else(|| text_field(rec, "stopReason"))
                    .unwrap_or_default();
                let mut extra = Map::new();
                extra.insert("kind".into(), Value::from("system"));
                if let Some(subtype) = text_field(rec, "subtype") {
                    extra.insert("subtype".into(), Value::from(subtype));
                }
                let mut event = make("meta", &text);
                event.extra = extra;
                vec![event]
            }
            Some("summary") => match text_field(rec, "summary") {
                Some(summary) => {
                    let mut event = make("meta", &summary);
                    event.extra.insert("kind".into(), Value::from("summary"));
                    vec![event]
                }
                None => Vec::new(),
            },
            _ => Vec::new(),
        }
    }
}

fn map_user(rec: &Value, make: &dyn Fn(&str, &str) -> RawEvent) -> Vec<RawEvent> {
    let Some(msg) = rec.get("message").filter(|value| value.is_object()) else {
        return Vec::new();
    };
    if msg.get("role").and_then(Value::as_str) != Some("user") {
        return Vec::new();
    }
    // isMeta marks injected content (hook feedback, command wrappers), not a human turn.
    let mut event_type = "user";
    let mut flag = Map::new();
    if rec.get("isSidechain") == Some(&Value::Bool(true)) {
        flag.insert("sidechain".into(), Value::Bool(true));
    }
    if rec.get("isMeta") == Some(&Value::Bool(true)) {
        event_type = "meta";
        flag.insert("kind".into(), Value::from("injected"));
    }
    let mut events = Vec::new();
    match msg.get("content") {
        Some(Value::String(content)) => {
            if !content.trim().is_empty() {
                let mut event = make(event_type, content);
                event.extra = flag;
                events.push(event);
            }
            events
        }
        Some(Value::Array(blocks)) => {
            let mut text_parts: Vec<String> = Vec::new();
            for block in blocks {
                if !block.is_object() {
                    continue;
                }
                match block.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        if let Some(Value::String(text)) = block.get("text") {
                            text_parts.push(text.clone());
                        }
                    }
                    Some("image") => text_parts.push("[image]".to_string()),
                    Some("tool_result") => events.push(tool_result_event(block, make, &flag)),
                    _ => {}
                }
            }
            let text = text_parts.join("\n").trim().to_string();
            if !text.is_empty() {
                let mut event = make(event_type, &text);
                event.extra = flag;
                events.push(event);
            }
            events
        }
        _ => Vec::new(),
    }
}

fn tool_result_event(
    block: &Value,
    make: &dyn Fn(&str, &str) -> RawEvent,
    flag: &Map<String, Value>,
) -> RawEvent {
    let text = match block.get("content") {
        Some(Value::String(content)) => content.clone(),
        Some(Value::Array(items)) => {
            let mut parts = Vec::new();
            for item in items {
                match item.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        if let Some(Value::String(text)) = item.get("text") {
                            parts.push(text.clone());
                        }
                    }
                    Some("image") => parts.push("[image]".to_string()),
                    _ => {}
                }
            }
            parts.join("\n")
        }
        _ => String::new(),
    };
    let mut extra = flag.clone();
    if let Some(tool_use_id) = text_field(block, "tool_use_id") {
        extra.insert("tool_use_id".into(), Value::from(tool_use_id));
    }
    if block.get("is_error") == Some(&Value::Bool(true)) {
        extra.insert("is_error".into(), Value::Bool(true));
    }
    if let Some(reference) = persisted_output_path(&text) {
        extra.insert("result_file".into(), Value::from(reference));
    }
    let mut event = make("tool_result", &text);
    event.extra = extra;
    event
}

/// Large tool results are persisted beside the transcript and referenced inline as
/// `"<persisted-output>\nOutput too large (…). Full output saved to: /abs/path.txt\n…"`.
/// We record the reference path only and never follow it.
fn persisted_output_path(text: &str) -> Option<String> {
    if !text.contains("<persisted-output>") {
        return None;
    }
    let marker = "saved to: ";
    let at = text.find(marker)?;
    let start = at + marker.len();
    let stop = text[start..]
        .find('\n')
        .map_or(text.len(), |offset| start + offset);
    let reference = text[start..stop].trim();
    (!reference.is_empty()).then(|| reference.to_string())
}

mod assistant;

use assistant::{attach_usage, map_assistant};
