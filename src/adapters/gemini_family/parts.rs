//! Google GenAI content, as Gemini CLI and its forks record it: a
//! `PartListUnion` is a string, one part or a list of parts, and a `Content`
//! is `{role, parts}`. A part is `{text}` (with `thought: true` it is the
//! model's reasoning), `{functionCall: {id?, name, args}}`,
//! `{functionResponse: {id?, name, response}}`, or inline/file data.
use serde_json::{Map, Value};

use crate::adapters::common::{as_text, event, meta, text_field, tool_call, tool_result, usage};
use crate::types::{ParserCtx, RawEvent};

/// The parts a `PartListUnion` or `Content` holds.
pub(super) fn parts(content: Option<&Value>) -> Vec<Value> {
    match content {
        Some(Value::String(text)) => vec![serde_json::json!({ "text": text })],
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| match item {
                Value::String(text) => serde_json::json!({ "text": text }),
                other => other.clone(),
            })
            .collect(),
        Some(Value::Object(map)) => match map.get("parts") {
            Some(Value::Array(parts)) => parts.clone(),
            _ => vec![Value::Object(map.clone())],
        },
        _ => Vec::new(),
    }
}

/// The events of parts spoken by `speaker` (`user` or `assistant`).
pub(super) fn part_events(
    ctx: &ParserCtx,
    speaker: &str,
    parts: &[Value],
    ts: Option<String>,
) -> Vec<RawEvent> {
    let mut events = Vec::new();
    for part in parts {
        if let Some(text) = text_field(part, "text") {
            let thought = part.get("thought").and_then(Value::as_bool) == Some(true);
            events.push(event(
                ctx,
                ts.clone(),
                if thought { "thinking" } else { speaker },
                text,
            ));
        } else if let Some(call) = part.get("functionCall") {
            events.push(tool_call(
                ctx,
                ts.clone(),
                call.get("name").and_then(Value::as_str),
                as_text(call.get("args")),
                call.get("id").and_then(Value::as_str),
            ));
        } else if let Some(response) = part.get("functionResponse") {
            events.push(tool_result(
                ctx,
                ts.clone(),
                response.get("name").and_then(Value::as_str),
                as_text(response.get("response")),
                response.get("id").and_then(Value::as_str),
                None,
            ));
        } else if let Some(data) = part.get("inlineData").or_else(|| part.get("fileData")) {
            let mut fields = Map::new();
            if let Some(mime) = data.get("mimeType") {
                fields.insert("mime_type".into(), mime.clone());
            }
            events.push(meta(ctx, ts.clone(), "attachment", String::new(), fields));
        }
    }
    events
}

/// The usage a `GenerateContentResponseUsageMetadata` reports.
pub(super) fn usage_metadata(
    ctx: &ParserCtx,
    metadata: &Value,
    model: Option<String>,
    ts: Option<String>,
) -> RawEvent {
    let count = |key: &str| metadata.get(key).and_then(Value::as_i64);
    usage(
        ctx,
        ts,
        model,
        count("promptTokenCount"),
        count("candidatesTokenCount"),
    )
}
