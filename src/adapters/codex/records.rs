//! Turning one recorded line into events: the parser that carries the session,
//! project and model it has seen so far, and what each record type contributes,
//! including the tool output a reply only references.

use serde_json::{Map, Value};

use crate::types::{Parser, ParserCtx, RawEvent};

use super::{cap, num_or_zero, text_field};

pub(super) struct CodexParser {
    session_id: Option<String>,
    project: Option<String>,
    model: Option<String>,
    last_ts: Option<String>,
}

impl CodexParser {
    /// A session id or project the caller already knows; an empty string is
    /// no answer at all and is treated as unknown, exactly as the driver did.
    pub(super) fn new(ctx: ParserCtx) -> Self {
        Self {
            session_id: ctx.session_id.filter(|value| !value.is_empty()),
            project: ctx.project.filter(|value| !value.is_empty()),
            model: None,
            last_ts: None,
        }
    }
}

impl Parser for CodexParser {
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

impl CodexParser {
    fn map_record(&mut self, rec: &Value) -> Vec<RawEvent> {
        if let Some(timestamp) = text_field(rec, "timestamp") {
            self.last_ts = Some(timestamp);
        }
        let Some(ts) = self.last_ts.clone() else {
            return Vec::new();
        };
        let Some(payload) = rec
            .get("payload")
            .filter(|value| value.is_object())
            .cloned()
        else {
            return Vec::new();
        };
        let record_type = rec
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if record_type == "session_meta" {
            if let Some(id) = text_field(&payload, "id") {
                self.session_id = Some(id);
            }
            if let Some(cwd) = text_field(&payload, "cwd") {
                self.project = Some(cwd);
            }
            let make = self.maker(&ts);
            let mut extra = Map::new();
            extra.insert("kind".into(), Value::from("session_meta"));
            if let Some(originator) = text_field(&payload, "originator") {
                extra.insert("originator".into(), Value::from(originator));
            }
            if let Some(version) = text_field(&payload, "cli_version") {
                extra.insert("cli_version".into(), Value::from(version));
            }
            if let Some(source) = text_field(&payload, "source") {
                extra.insert("source".into(), Value::from(source));
            }
            let mut event = make("meta", "");
            event.extra = extra;
            return vec![event];
        }
        if record_type == "turn_context" {
            // Pure configuration: refresh session state, emit nothing.
            if let Some(model) = text_field(&payload, "model") {
                self.model = Some(model);
            }
            if let Some(cwd) = text_field(&payload, "cwd") {
                self.project = Some(cwd);
            }
            return Vec::new();
        }
        if record_type == "event_msg" {
            return self.map_event_msg(&payload, &ts);
        }
        if record_type == "response_item" {
            return self.map_response_item(&payload, &ts);
        }
        if record_type == "compacted" {
            let make = self.maker(&ts);
            let text = text_field(&payload, "message").unwrap_or_default();
            let mut event = make("meta", &text);
            event.extra.insert("kind".into(), Value::from("compacted"));
            return vec![event];
        }
        Vec::new()
    }

    fn maker(&self, ts: &str) -> impl Fn(&str, &str) -> RawEvent {
        let ts = ts.to_string();
        let session_id = self.session_id.clone();
        let project = self.project.clone();
        move |event_type: &str, text: &str| RawEvent {
            ts: Some(ts.clone()),
            session_id: session_id.clone(),
            project: project.clone(),
            event_type: event_type.to_string(),
            text: cap(text),
            ..RawEvent::default()
        }
    }

    fn map_event_msg(&mut self, payload: &Value, ts: &str) -> Vec<RawEvent> {
        let make = self.maker(ts);
        match payload.get("type").and_then(Value::as_str) {
            Some("user_message") => {
                let text = text_field(payload, "message").unwrap_or_default();
                vec![make("user", &text)]
            }
            Some("token_count") => {
                let Some(info) = payload.get("info").filter(|value| value.is_object()) else {
                    return Vec::new();
                };
                let Some(usage) = info
                    .get("last_token_usage")
                    .filter(|value| value.is_object())
                else {
                    return Vec::new();
                };
                let total_input = num_or_zero(usage, "input_tokens");
                let cached_input = num_or_zero(usage, "cached_input_tokens");
                let output = num_or_zero(usage, "output_tokens");
                let reasoning_output = num_or_zero(usage, "reasoning_output_tokens");
                if total_input + output + reasoning_output == 0 {
                    return Vec::new();
                }
                let mut event = make("meta", "");
                event.model = self.model.clone();
                event.tokens_in = Some(total_input);
                event.tokens_out = Some(output + reasoning_output);
                event
                    .extra
                    .insert("kind".into(), Value::from("token_count"));
                event.extra.insert(
                    "input_non_cached_tokens".into(),
                    Value::from((total_input - cached_input).max(0)),
                );
                event
                    .extra
                    .insert("cache_creation_tokens".into(), Value::from(0));
                event
                    .extra
                    .insert("cache_read_tokens".into(), Value::from(cached_input));
                vec![event]
            }
            // agent_message / agent_reasoning duplicate response_item content;
            // task_started, task_complete and the rest are turn bookkeeping. All dropped.
            _ => Vec::new(),
        }
    }

    fn map_response_item(&mut self, payload: &Value, ts: &str) -> Vec<RawEvent> {
        let Some(sub) = text_field(payload, "type") else {
            return Vec::new();
        };
        let make = self.maker(ts);
        if sub == "message" {
            return self.map_message_item(payload, ts);
        }
        if sub == "reasoning" {
            // summary carries the readable text; encrypted_content is opaque and skipped.
            let mut parts = Vec::new();
            if let Some(Value::Array(items)) = payload.get("summary") {
                for item in items {
                    if let Some(Value::String(text)) = item.get("text") {
                        if !text.trim().is_empty() {
                            parts.push(text.clone());
                        }
                    }
                }
            }
            let text = parts.join("\n");
            if text.is_empty() {
                return Vec::new();
            }
            let mut event = make("thinking", &text);
            event.model = self.model.clone();
            return vec![event];
        }
        if sub.ends_with("_call_output") {
            let mut extra = Map::new();
            extra.insert("kind".into(), Value::from(sub.clone()));
            if let Some(call_id) = text_field(payload, "call_id") {
                extra.insert("call_id".into(), Value::from(call_id));
            }
            let mut event = make("tool_result", &output_text(payload.get("output")));
            event.extra = extra;
            return vec![event];
        }
        if sub.ends_with("_call") {
            let mut args = text_field(payload, "arguments")
                .or_else(|| text_field(payload, "input"))
                .unwrap_or_default();
            if args.is_empty() {
                if let Some(action) = payload.get("action") {
                    args = serde_json::to_string(action).unwrap_or_default();
                }
            }
            let mut extra = Map::new();
            extra.insert("kind".into(), Value::from(sub.clone()));
            if let Some(call_id) = text_field(payload, "call_id") {
                extra.insert("call_id".into(), Value::from(call_id));
            }
            let tool_name = text_field(payload, "name").unwrap_or(sub);
            let mut event = make("tool_call", &args);
            event.tool_name = Some(tool_name);
            event.model = self.model.clone();
            event.extra = extra;
            return vec![event];
        }
        Vec::new()
    }

}

mod message;

use message::output_text;
