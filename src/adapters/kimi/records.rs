//! Turning one recorded line into events: the parser that carries the session,
//! project and timestamp it has seen so far, and what each record and content
//! block contributes, down to the tool result that reports its own failure.

use std::collections::HashMap;

use serde_json::{Map, Value};

use crate::types::{Parser, ParserCtx, RawEvent};

use super::{cap, iso_from, num_or_zero, text_field};

mod message;

use message::{is_truthy, map_message};

pub(super) struct KimiParser {
    session_id: Option<String>,
    project: Option<String>,
    last_ts: Option<String>,
    last_model: Option<String>,
    tool_names: HashMap<String, String>,
}

impl KimiParser {
    /// A session id or project the caller already knows; an empty string is
    /// no answer at all and is treated as unknown, exactly as the driver did.
    /// Tool names arrive with the call and are needed again at its result.
    pub(super) fn new(ctx: ParserCtx) -> Self {
        Self {
            session_id: ctx.session_id.filter(|value| !value.is_empty()),
            project: ctx.project.filter(|value| !value.is_empty()),
            last_ts: None,
            last_model: None,
            tool_names: HashMap::new(),
        }
    }
}

impl Parser for KimiParser {
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

impl KimiParser {
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

    fn map_record(&mut self, rec: &Value) -> Vec<RawEvent> {
        let ts = iso_from(rec.get("time"))
            .or_else(|| iso_from(rec.get("created_at")))
            .or_else(|| self.last_ts.clone());
        let Some(ts) = ts.filter(|value| !value.is_empty()) else {
            return Vec::new();
        };
        self.last_ts = Some(ts.clone());
        let record_type = rec.get("type").and_then(Value::as_str).unwrap_or_default();
        if record_type == "context.append_loop_event" {
            return self.map_loop_event(rec.get("event"), &ts);
        }
        if record_type == "context.append_message" {
            return map_message(rec.get("message"), &self.maker(&ts));
        }
        if record_type == "usage.record" {
            return self.map_usage(rec, &ts);
        }
        let make = self.maker(&ts);
        if record_type == "metadata" {
            let mut extra = Map::new();
            extra.insert("kind".into(), Value::from("metadata"));
            if let Some(protocol) = text_field(rec, "protocol_version") {
                extra.insert("protocol".into(), Value::from(protocol));
            }
            if let Some(app) = text_field(rec, "app_version") {
                extra.insert("app".into(), Value::from(app));
            }
            let mut event = make("meta", "");
            event.extra = extra;
            return vec![event];
        }
        if record_type == "config.update" {
            // The systemPrompt payload is deliberately dropped: meta at most, no text.
            let mut extra = Map::new();
            extra.insert("kind".into(), Value::from("config"));
            if let Some(profile) = text_field(rec, "profileName") {
                extra.insert("profile".into(), Value::from(profile));
            }
            let mut event = make("meta", "");
            event.extra = extra;
            return vec![event];
        }
        if record_type == "context.apply_compaction" {
            if let Some(summary) = text_field(rec, "summary") {
                let mut event = make("meta", &summary);
                event.extra.insert("kind".into(), Value::from("compaction"));
                return vec![event];
            }
            return Vec::new();
        }
        if record_type == "turn.cancel" {
            let mut event = make("meta", "");
            event
                .extra
                .insert("kind".into(), Value::from("turn.cancel"));
            return vec![event];
        }
        // turn.prompt / turn.steer echo append_message; tools.*, permission.*, *_mode.*,
        // and compaction bookkeeping records carry no conversational content.
        Vec::new()
    }

    fn map_loop_event(&mut self, event: Option<&Value>, ts: &str) -> Vec<RawEvent> {
        let Some(ev) = event.filter(|value| value.is_object()) else {
            return Vec::new();
        };
        let make = self.maker(ts);
        match ev.get("type").and_then(Value::as_str) {
            Some("content.part") => {
                let Some(part) = ev.get("part").filter(|value| value.is_object()) else {
                    return Vec::new();
                };
                match part.get("type").and_then(Value::as_str) {
                    Some("text") => match text_field(part, "text") {
                        Some(text) => {
                            let mut out = make("assistant", &text);
                            out.model = self.last_model.clone();
                            vec![out]
                        }
                        None => Vec::new(),
                    },
                    Some("think") => match text_field(part, "think") {
                        Some(text) => {
                            let mut out = make("thinking", &text);
                            out.model = self.last_model.clone();
                            vec![out]
                        }
                        None => Vec::new(),
                    },
                    _ => Vec::new(),
                }
            }
            Some("tool.call") => {
                let call_id = text_field(ev, "toolCallId").or_else(|| text_field(ev, "uuid"));
                let name = text_field(ev, "name");
                if let (Some(call_id), Some(name)) = (&call_id, &name) {
                    self.tool_names.insert(call_id.clone(), name.clone());
                }
                // Mirror the claude adapter: the argument JSON is the searchable text.
                let mut text = match ev.get("args") {
                    Some(args) => serde_json::to_string(args).unwrap_or_default(),
                    None => String::new(),
                };
                if text.is_empty() {
                    if let Some(description) = text_field(ev, "description") {
                        text = description;
                    }
                }
                let mut extra = Map::new();
                if let Some(call_id) = &call_id {
                    extra.insert("tool_use_id".into(), Value::from(call_id.clone()));
                }
                let mut out = make("tool_call", &text);
                out.tool_name = name;
                out.model = self.last_model.clone();
                out.extra = extra;
                vec![out]
            }
            Some("tool.result") => {
                let call_id = text_field(ev, "toolCallId").or_else(|| text_field(ev, "parentUuid"));
                let result = ev.get("result").filter(|value| value.is_object());
                let text = match result.and_then(|result| result.get("output")) {
                    Some(Value::String(output)) => output.clone(),
                    _ => match ev.get("result") {
                        Some(Value::String(output)) => output.clone(),
                        _ => String::new(),
                    },
                };
                let tool_name = call_id
                    .as_ref()
                    .and_then(|call_id| self.tool_names.get(call_id).cloned());
                let mut extra = Map::new();
                if let Some(call_id) = &call_id {
                    extra.insert("tool_use_id".into(), Value::from(call_id.clone()));
                }
                let is_error = result
                    .and_then(|result| result.get("isError"))
                    .is_some_and(is_truthy);
                if is_error {
                    extra.insert("is_error".into(), Value::Bool(true));
                }
                let mut out = make("tool_result", &text);
                out.tool_name = tool_name;
                out.extra = extra;
                vec![out]
            }
            // step.begin/step.end carry loop bookkeeping only; usage.record owns tokens.
            _ => Vec::new(),
        }
    }

    /// 'turn'-scoped usage records are per-step deltas (verified identical to step.end
    /// usage), so summing them downstream yields honest session totals. 'session'-scoped
    /// snapshots would double count and only refresh the current model name.
    fn map_usage(&mut self, rec: &Value, ts: &str) -> Vec<RawEvent> {
        if let Some(model) = text_field(rec, "model") {
            self.last_model = Some(model);
        }
        if rec.get("usageScope").and_then(Value::as_str) != Some("turn") {
            return Vec::new();
        }
        let empty = Value::Object(Map::new());
        let usage = rec
            .get("usage")
            .filter(|value| value.is_object())
            .unwrap_or(&empty);
        let input_other = num_or_zero(usage, "inputOther");
        let cache_read = num_or_zero(usage, "inputCacheRead");
        let cache_creation = num_or_zero(usage, "inputCacheCreation");
        let tokens_in = input_other + cache_read + cache_creation;
        let tokens_out = num_or_zero(usage, "output");
        if tokens_in + tokens_out == 0 {
            return Vec::new();
        }
        let mut event = self.maker(ts)("meta", "");
        event.model = self.last_model.clone();
        event.tokens_in = Some(tokens_in);
        event.tokens_out = Some(tokens_out);
        event.extra.insert("kind".into(), Value::from("usage"));
        event
            .extra
            .insert("input_non_cached_tokens".into(), Value::from(input_other));
        event
            .extra
            .insert("cache_creation_tokens".into(), Value::from(cache_creation));
        event
            .extra
            .insert("cache_read_tokens".into(), Value::from(cache_read));
        vec![event]
    }
}
