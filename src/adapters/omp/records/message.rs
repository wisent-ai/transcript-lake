//! What one message record contributes: its content blocks in order, the tool
//! calls and results among them, and the token spend recorded beside them.

use serde_json::Value;

use crate::types::RawEvent;

use super::OmpParser;
use crate::adapters::common::{int_field, js_string, prune, text_joined, Usage};

impl OmpParser {
    pub(super) fn message_events(&self, rec: &Value, ts: Option<&String>) -> Vec<RawEvent> {
        let Some(msg) = rec.get("message").filter(|value| value.is_object()) else {
            let mut event = self.make(ts, "meta", None);
            event.extra.insert("kind".into(), Value::from("message"));
            return vec![event];
        };
        let role = match msg.get("role") {
            Some(Value::String(role)) => role.clone(),
            _ => "unknown".to_string(),
        };
        if role == "toolResult" {
            let output = text_joined(msg.get("content"));
            let mut event = self.make(ts, "tool_result", Some(&output));
            event.tool_name = match msg.get("toolName") {
                Some(Value::String(name)) => Some(name.clone()),
                _ => None,
            };
            prune(&mut event.extra, "call_id", msg.get("toolCallId"));
            if msg.get("isError") == Some(&Value::Bool(true)) {
                event.extra.insert("is_error".into(), Value::Bool(true));
            }
            return vec![event];
        }
        let attribution = msg.get("attribution").and_then(Value::as_str);
        let generated = msg.get("synthetic") == Some(&Value::Bool(true))
            || msg.get("steering") == Some(&Value::Bool(true));
        let text_type = match role.as_str() {
            "user"
                if attribution == Some("agent") || (generated && attribution != Some("user")) =>
            {
                "meta"
            }
            "user" => "user",
            "assistant" => "assistant",
            _ => "meta",
        };
        let mut events: Vec<RawEvent> = Vec::new();
        let mut buffer: Vec<String> = Vec::new();
        let owned;
        let blocks: &[Value] = match msg.get("content") {
            Some(Value::Array(blocks)) => blocks,
            other => {
                owned = vec![serde_json::json!({ "type": "text", "text": text_joined(other) })];
                &owned
            }
        };
        for block in blocks {
            if !block.is_object() {
                continue;
            }
            match block.get("type").and_then(Value::as_str) {
                Some("text") => {
                    // A text block without text adds no words.
                    if let Some(Value::String(text)) = block.get("text") {
                        buffer.push(text.clone());
                    }
                }
                Some("thinking") => {
                    self.flush_text(&mut events, &mut buffer, ts, text_type, &role);
                    let thinking = block.get("thinking").and_then(Value::as_str);
                    events.push(self.make(ts, "thinking", thinking));
                }
                Some("toolCall") => {
                    self.flush_text(&mut events, &mut buffer, ts, text_type, &role);
                    // The call's arguments are the row's text, as the claude
                    // adapter writes a tool_use block's input: a reader of the
                    // projection sees what the agent ran, not only that it ran
                    // something. `arguments` is already JSON; its text form is
                    // the call's arguments, and a call without them has none.
                    let args = block.get("arguments").map(Value::to_string);
                    let mut event = self.make(ts, "tool_call", args.as_deref());
                    event.tool_name = match block.get("name") {
                        Some(Value::String(name)) => Some(name.clone()),
                        _ => None,
                    };
                    prune(&mut event.extra, "call_id", block.get("id"));
                    events.push(event);
                }
                _ => {
                    self.flush_text(&mut events, &mut buffer, ts, text_type, &role);
                    let mut event = self.make(ts, "meta", None);
                    event.extra.insert("kind".into(), Value::from("block"));
                    event.extra.insert(
                        "omp_block".into(),
                        Value::from(js_string(block.get("type"))),
                    );
                    events.push(event);
                }
            }
        }
        self.flush_text(&mut events, &mut buffer, ts, text_type, &role);
        for event in &mut events {
            prune(&mut event.extra, "attribution", msg.get("attribution"));
            prune(&mut event.extra, "steering", msg.get("steering"));
            prune(&mut event.extra, "synthetic", msg.get("synthetic"));
        }
        if role == "assistant" {
            let model = match msg.get("model") {
                Some(Value::String(model)) => Some(model.clone()),
                _ => self.model.clone(),
            };
            for event in &mut events {
                event.model = model.clone();
            }
            // OMP's usage keeps the fresh input apart from the cache it
            // wrote and read; they travel as every adapter's usage does,
            // beside the credential the call was billed to.
            if let Some(head) = events.first_mut() {
                if let Some(usage) = msg.get("usage").filter(|value| value.is_object()) {
                    Usage {
                        fresh: int_field(usage, "input"),
                        cache_write: int_field(usage, "cacheWrite"),
                        cache_read: int_field(usage, "cacheRead"),
                        output: int_field(usage, "output"),
                    }
                    .record(head);
                }
                if let Some(credential) = msg.get("credentialId").and_then(Value::as_i64) {
                    head.extra
                        .insert("credential_id".into(), Value::from(credential));
                }
            }
        }
        events
    }
}
