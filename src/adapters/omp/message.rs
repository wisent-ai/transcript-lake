//! What one message record contributes: its content blocks in order, the tool
//! calls and results among them, and the token spend recorded beside them.

use serde_json::{Map, Value};

use crate::types::RawEvent;

use super::super::{clip, js_string, number, prune, text_of};
use super::OmpParser;

impl OmpParser {
    fn message_events(&self, rec: &Value, ts: Option<&String>) -> Vec<RawEvent> {
        let Some(msg) = rec.get("message").filter(|value| value.is_object()) else {
            let mut event = self.make(ts, "meta", "");
            event.extra.insert("kind".into(), Value::from("message"));
            return vec![event];
        };
        let role = match msg.get("role") {
            Some(Value::String(role)) => role.clone(),
            _ => "unknown".to_string(),
        };
        if role == "toolResult" {
            let mut event = self.make(ts, "tool_result", &text_of(msg.get("content")));
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
        let text_type = match role.as_str() {
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
                owned = vec![serde_json::json!({ "type": "text", "text": text_of(other) })];
                &owned
            }
        };
        for block in blocks {
            if !block.is_object() {
                continue;
            }
            match block.get("type").and_then(Value::as_str) {
                Some("text") => {
                    buffer.push(match block.get("text") {
                        Some(Value::String(text)) => text.clone(),
                        _ => String::new(),
                    });
                }
                Some("thinking") => {
                    self.flush_text(&mut events, &mut buffer, ts, text_type, &role);
                    let thinking = match block.get("thinking") {
                        Some(Value::String(text)) => text.clone(),
                        _ => String::new(),
                    };
                    events.push(self.make(ts, "thinking", &thinking));
                }
                Some("toolCall") => {
                    self.flush_text(&mut events, &mut buffer, ts, text_type, &role);
                    // The call's arguments are the row's text, as the claude
                    // adapter writes a tool_use block's input: a reader of the
                    // projection sees what the agent ran, not only that it ran
                    // something. `arguments` came out of JSON parsing, so
                    // serialization cannot fail.
                    let args = match block.get("arguments") {
                        Some(arguments) => serde_json::to_string(arguments).unwrap_or_default(),
                        None => String::new(),
                    };
                    let mut event = self.make(ts, "tool_call", &args);
                    event.tool_name = match block.get("name") {
                        Some(Value::String(name)) => Some(name.clone()),
                        _ => None,
                    };
                    prune(&mut event.extra, "call_id", block.get("id"));
                    events.push(event);
                }
                _ => {
                    self.flush_text(&mut events, &mut buffer, ts, text_type, &role);
                    let mut event = self.make(ts, "meta", "");
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
        if role == "assistant" {
            let model = match msg.get("model") {
                Some(Value::String(model)) => Some(model.clone()),
                _ => self.model.clone(),
            };
            for event in &mut events {
                event.model = model.clone();
            }
            if let Some(head) = events.first_mut() {
                if let Some(usage) = msg.get("usage").filter(|value| value.is_object()) {
                    if let Some(tokens) = usage.get("input").and_then(number) {
                        head.tokens_in = Some(tokens);
                    }
                    if let Some(tokens) = usage.get("output").and_then(number) {
                        head.tokens_out = Some(tokens);
                    }
                }
            }
        }
        events
    }

}
