//! What one message item contributes: the role it carries, its content and
//! the reasoning summary beside it, and the tool output a reply only refers
//! to, which may arrive as a plain string, a JSON string or an object.

use serde_json::Value;

use crate::types::RawEvent;

use super::super::{cap, text_field};
use super::CodexParser;

impl CodexParser {
    fn map_message_item(&mut self, payload: &Value, ts: &str) -> Vec<RawEvent> {
        let Some(role) = text_field(payload, "role") else {
            return Vec::new();
        };
        // Role user duplicates event_msg/user_message and additionally carries injected
        // environment/permission context — dropped here to keep user turns clean.
        if role == "user" {
            return Vec::new();
        }
        let mut parts = Vec::new();
        if let Some(Value::Array(blocks)) = payload.get("content") {
            for block in blocks {
                if !block.is_object() {
                    continue;
                }
                if let Some(Value::String(text)) = block.get("text") {
                    if !text.trim().is_empty() {
                        parts.push(text.clone());
                    }
                }
            }
        }
        let text = parts.join("\n");
        let make = self.maker(ts);
        if role == "assistant" {
            if text.is_empty() {
                return Vec::new();
            }
            let mut event = make("assistant", &text);
            event.model = self.model.clone();
            return vec![event];
        }
        // developer / system prompts: record presence only, never the text.
        let mut event = make("meta", "");
        event
            .extra
            .insert("kind".into(), Value::from("system_prompt"));
        event.extra.insert("role".into(), Value::from(role));
        vec![event]
    }
}

/// `function_call_output.output` is usually a plain string; some CLI versions wrap it
/// as a JSON string or object `{ output | content, metadata }`. Unwrap the readable
/// part. A leading '{' does not guarantee JSON: the raw string IS the tool output then.
pub(super) fn output_text(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => {
            let trimmed = text.trim();
            if trimmed.starts_with('{') && trimmed.contains("\"output\"") {
                if let Ok(parsed) = serde_json::from_str::<Value>(trimmed) {
                    if let Some(Value::String(output)) = parsed.get("output") {
                        return output.clone();
                    }
                }
            }
            text.clone()
        }
        Some(Value::Object(map)) => {
            if let Some(Value::String(output)) = map.get("output") {
                return output.clone();
            }
            if let Some(Value::String(content)) = map.get("content") {
                return content.clone();
            }
            serde_json::to_string(&Value::Object(map.clone())).unwrap_or_default()
        }
        Some(items @ Value::Array(_)) => serde_json::to_string(items).unwrap_or_default(),
        _ => String::new(),
    }
}
