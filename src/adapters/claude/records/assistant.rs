//! What an assistant record contributes: the text and tool calls it carries,
//! and the token spend attached to it so a reply that only thought still
//! reports what it cost.

use serde_json::{Map, Value};

use crate::types::RawEvent;

use crate::adapters::common::{int_field, text_field, Usage};

pub(super) fn map_assistant(rec: &Value, make: &dyn Fn(&str, &str) -> RawEvent) -> Vec<RawEvent> {
    let Some(msg) = rec.get("message").filter(|value| value.is_object()) else {
        return Vec::new();
    };
    let model = text_field(msg, "model");
    let usage = msg.get("usage").filter(|value| value.is_object());
    let mut flag = Map::new();
    if rec.get("isSidechain") == Some(&Value::Bool(true)) {
        flag.insert("sidechain".into(), Value::Bool(true));
    }
    let owned;
    let blocks: &[Value] = match msg.get("content") {
        Some(Value::Array(blocks)) => blocks,
        Some(Value::String(content)) => {
            owned = vec![serde_json::json!({ "type": "text", "text": content })];
            &owned
        }
        _ => &[],
    };
    let mut events = Vec::new();
    for block in blocks {
        if !block.is_object() {
            continue;
        }
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(Value::String(text)) = block.get("text") {
                    if !text.trim().is_empty() {
                        let mut event = make("assistant", text);
                        event.model = model.clone();
                        event.extra = flag.clone();
                        events.push(event);
                    }
                }
            }
            Some("thinking") => {
                // Encrypted-only thinking blocks carry an empty string plus a
                // signature; skip those.
                if let Some(Value::String(thinking)) = block.get("thinking") {
                    if !thinking.trim().is_empty() {
                        let mut event = make("thinking", thinking);
                        event.model = model.clone();
                        event.extra = flag.clone();
                        events.push(event);
                    }
                }
            }
            Some("tool_use") => {
                // block.input is already JSON; its text form is the call's arguments.
                let args = match block.get("input") {
                    Some(input) => input.to_string(),
                    None => String::new(),
                };
                let mut extra = flag.clone();
                if let Some(id) = text_field(block, "id") {
                    extra.insert("tool_use_id".into(), Value::from(id));
                }
                let mut event = make("tool_call", &args);
                event.tool_name = text_field(block, "name");
                event.model = model.clone();
                event.extra = extra;
                events.push(event);
            }
            _ => {}
        }
    }
    attach_usage(&mut events, usage, model.as_deref(), make);
    events
}

/// Usage is reported once per assistant record; attach it to the first emitted event so
/// downstream aggregation never double counts. Records whose only content is encrypted
/// thinking still surface their token spend through a small meta event.
pub(super) fn attach_usage(
    events: &mut Vec<RawEvent>,
    usage: Option<&Value>,
    model: Option<&str>,
    make: &dyn Fn(&str, &str) -> RawEvent,
) {
    let Some(usage) = usage else {
        return;
    };
    let usage = Usage {
        fresh: int_field(usage, "input_tokens"),
        cache_write: int_field(usage, "cache_creation_input_tokens"),
        cache_read: int_field(usage, "cache_read_input_tokens"),
        output: int_field(usage, "output_tokens"),
    };
    if !usage.is_reported() {
        return;
    }
    if let Some(first) = events.first_mut() {
        usage.record(first);
        return;
    }
    let mut event = make("meta", "");
    event.model = model.map(str::to_string);
    event.extra.insert("kind".into(), Value::from("usage"));
    usage.record(&mut event);
    events.push(event);
}
