//! What one message contributes: its content blocks in order, and the tool
//! call and result among them, including a result that reports its own
//! failure through a flag the driver read as JavaScript truthiness.

use serde_json::{Map, Value};

use crate::types::RawEvent;

use super::super::cap;


/// JS truthiness for the `result.isError` guard, which was a bare `if (result.isError)`.
pub(super) fn is_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number
            .as_f64()
            .is_some_and(|raw| raw != 0.0 && !raw.is_nan()),
        Value::String(text) => !text.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

pub(super) fn map_message(msg: Option<&Value>, make: &dyn Fn(&str, &str) -> RawEvent) -> Vec<RawEvent> {
    let Some(msg) = msg.filter(|value| value.is_object()) else {
        return Vec::new();
    };
    let Some(role) = text_field(msg, "role") else {
        return Vec::new();
    };
    let mut parts = Vec::new();
    match msg.get("content") {
        Some(Value::Array(blocks)) => {
            for block in blocks {
                if block.get("type").and_then(Value::as_str) == Some("text") {
                    if let Some(Value::String(text)) = block.get("text") {
                        parts.push(text.clone());
                    }
                }
            }
        }
        Some(Value::String(content)) => parts.push(content.clone()),
        _ => {}
    }
    let text = parts.join("\n").trim().to_string();
    if text.is_empty() {
        return Vec::new();
    }
    let origin = msg
        .get("origin")
        .filter(|value| value.is_object())
        .and_then(|origin| text_field(origin, "kind"));
    if role == "user" {
        if let Some(origin) = origin.filter(|origin| origin != "user") {
            // Injections and background-task notifications are synthetic context,
            // not turns.
            let mut event = make("meta", &text);
            event.extra.insert("kind".into(), Value::from("injected"));
            event.extra.insert("origin".into(), Value::from(origin));
            return vec![event];
        }
        return vec![make("user", &text)];
    }
    if role == "assistant" {
        return vec![make("assistant", &text)];
    }
    Vec::new()
}
