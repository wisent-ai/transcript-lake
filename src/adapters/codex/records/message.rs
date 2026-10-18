//! What one message item contributes: the role it carries, its content and
//! the reasoning summary beside it, and the tool output a reply only refers
//! to, which may arrive as a plain string, a JSON string or an object.

use serde_json::Value;

use crate::types::RawEvent;

use super::CodexParser;
use crate::adapters::common::text_field;

impl CodexParser {
    pub(super) fn map_message_item(&mut self, payload: &Value, ts: &str) -> Vec<RawEvent> {
        let Some(role) = text_field(payload, "role") else {
            return Vec::new();
        };
        // Role user duplicates the event_msg user turn and additionally carries injected
        // environment/permission context, so it is no event of its own. Its time is kept:
        // it is the first record of the words a completed UserMessage item repeats.
        if role == "user" {
            self.first_user_record = Some((ts.to_string(), content_text(payload.get("content"))));
            return Vec::new();
        }
        let text = content_text(payload.get("content"));
        let make = self.maker(ts);
        if role == "assistant" {
            if text.is_empty() {
                return Vec::new();
            }
            let mut event = make("assistant", Some(&text));
            event.model = self.model.clone();
            return vec![event];
        }
        // developer / system prompts: record presence only, never the text.
        let mut event = make("meta", None);
        event
            .extra
            .insert("kind".into(), Value::from("system_prompt"));
        event.extra.insert("role".into(), Value::from(role));
        vec![event]
    }
}

/// The non-blank `text` of every object block in a Codex `content` array,
/// one per line; a response item's message and a completed `UserMessage`
/// item carry their words in the same shape.
pub(super) fn content_text(content: Option<&Value>) -> String {
    let Some(Value::Array(blocks)) = content else {
        return String::new();
    };
    blocks
        .iter()
        .filter_map(|block| match block.get("text") {
            Some(Value::String(text)) if !text.trim().is_empty() => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `function_call_output.output` is usually a plain string; some CLI versions wrap it
/// as a JSON string or object `{ output | content, metadata }`. Unwrap the readable
/// part. A leading '{' does not guarantee JSON: the raw string IS the tool output then.
/// An output the item does not carry is none.
pub(super) fn output_text(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(text) => {
            let trimmed = text.trim();
            if trimmed.starts_with('{') && trimmed.contains("\"output\"") {
                if let Ok(parsed) = serde_json::from_str::<Value>(trimmed) {
                    if let Some(Value::String(output)) = parsed.get("output") {
                        return Some(output.clone());
                    }
                }
            }
            Some(text.clone())
        }
        object @ Value::Object(map) => {
            if let Some(Value::String(output)) = map.get("output") {
                return Some(output.clone());
            }
            if let Some(Value::String(content)) = map.get("content") {
                return Some(content.clone());
            }
            Some(object.to_string())
        }
        items @ Value::Array(_) => Some(items.to_string()),
        _ => None,
    }
}
