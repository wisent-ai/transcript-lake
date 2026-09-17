//! One exported row: what a canonical Lake event becomes in the per-session
//! file Oko imports, plus the ordering, deduplication and rendering that file
//! depends on.

use std::collections::HashSet;

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

const FP_LEN: usize = 32;
const CONVERSATION_EVENTS: [&str; 6] = [
    "user",
    "assistant",
    "thinking",
    "tool_call",
    "tool_result",
    "meta",
];

/// `String(value)` as JavaScript performs it for the values a canonical row
/// can carry.
pub(crate) fn js_string(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(true) => "true".to_string(),
        Value::Bool(false) => "false".to_string(),
        Value::Number(number) => number.to_string(),
        Value::String(text) => text.clone(),
        Value::Array(items) => items
            .iter()
            .map(|item| {
                if item.is_null() {
                    String::new()
                } else {
                    js_string(item)
                }
            })
            .collect::<Vec<_>>()
            .join(","),
        Value::Object(_) => "[object Object]".to_string(),
    }
}

fn is_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number
            .as_f64()
            .is_some_and(|raw| raw != 0.0 && !raw.is_nan()),
        Value::String(text) => !text.is_empty(),
        _ => true,
    }
}

/// `String(value || '')`: an absent or falsy field contributes nothing.
fn coerce_text(value: Option<&Value>) -> String {
    match value {
        Some(value) if is_truthy(value) => js_string(value),
        _ => String::new(),
    }
}

/// 2^53: whole numbers below it survive a double round trip, so they are published as integers.
const MAX_EXACT_INTEGER: f64 = 9_007_199_254_740_992.0;

/// Whole numbers are published as JSON integers, matching how the previous
/// implementation serialized doubles that carry no fraction.
pub(crate) fn number_value(raw: f64) -> Value {
    if raw.is_finite() && raw.fract() == 0.0 && raw.abs() < MAX_EXACT_INTEGER {
        return Value::from(raw as i64);
    }
    Value::from(raw)
}

// Deterministic per-event ids deduplicate recovery replays and give Oko
// stable tool-use identifiers without retaining a source filename.
pub(crate) fn fingerprint(event: &Value, runtime: &str) -> String {
    let extra = match event.get("extra") {
        Some(extra) if is_truthy(extra) => {
            serde_json::to_string(extra).unwrap_or_else(|_| "{}".to_string())
        }
        _ => "{}".to_string(),
    };
    let mut hash = Sha256::new();
    let fields = [
        runtime.to_string(),
        coerce_text(event.get("session_id")),
        coerce_text(event.get("ts")),
        coerce_text(event.get("event_type")),
        coerce_text(event.get("text")),
        coerce_text(event.get("tool_name")),
        coerce_text(event.get("model")),
        extra,
    ];
    for field in fields {
        hash.update(field.as_bytes());
        hash.update(b"\n");
    }
    format!("{:x}", hash.finalize())[..FP_LEN].to_string()
}

fn optional_string(event: &Value, key: &str) -> Value {
    match event.get(key) {
        Some(Value::String(text)) => Value::String(text.clone()),
        _ => Value::Null,
    }
}

fn optional_number(event: &Value, key: &str) -> Value {
    match event.get(key) {
        Some(Value::Number(number)) => Value::Number(number.clone()),
        _ => Value::Null,
    }
}

/// The speaker of the turn, on the same line as its text.
///
/// A reader asking "did the operator say this?" looks for `role`, or
/// `message.role`, the shape every vendor transcript store writes. This
/// projection named the speaker only in `event_type`, and the write gate that
/// demands a recorded justification for a new test verifies the operator's
/// quote by reading Oko's index, opening the indexed file, and accepting the
/// quote only from a line whose role is `user`. Every session Oko indexes is
/// one of these projections, so that gate could accept no quote at all and the
/// registry it demands stayed unwritable. Only the two speaking events carry a
/// role: a tool call, a tool result, a thinking block and injected meta have
/// no speaker to name.
fn conversation_role(event: &Value) -> Value {
    match event.get("event_type").and_then(Value::as_str) {
        Some("user") => json!("user"),
        Some("assistant") => json!("assistant"),
        _ => Value::Null,
    }
}

/// The operator's words again, under the key a vendor-shaped reader expects.
///
/// The quote checker the write gate runs takes `message.content` or
/// `content` from the line and accepts the operator's quote only when it
/// finds the quote there, so naming the speaker is not enough on its own: a
/// user turn carries its text under that key too. An assistant turn, a tool
/// call, a tool result and a thinking block do not, because no reader asks
/// that question about them and these files are large.
fn operator_content(event: &Value) -> Value {
    match event.get("event_type").and_then(Value::as_str) {
        Some("user") => match event.get("text") {
            Some(Value::String(text)) => Value::String(text.clone()),
            _ => Value::Null,
        },
        _ => Value::Null,
    }
}

/// The exported row, in the field order Oko's importer reads.
pub(crate) fn export_line(event: &Value, runtime: &str, fingerprint: &str) -> Value {
    let mut row = Map::new();
    row.insert("lake_schema".to_string(), json!("oko-import-v1"));
    row.insert("uuid".to_string(), json!(fingerprint));
    row.insert(
        "ts".to_string(),
        event.get("ts").cloned().unwrap_or(Value::Null),
    );
    row.insert("runtime".to_string(), json!(runtime));
    row.insert(
        "session_id".to_string(),
        event.get("session_id").cloned().unwrap_or(Value::Null),
    );
    row.insert("project".to_string(), optional_string(event, "project"));
    row.insert(
        "event_type".to_string(),
        event.get("event_type").cloned().unwrap_or(Value::Null),
    );
    row.insert("role".to_string(), conversation_role(event));
    row.insert("content".to_string(), operator_content(event));
    row.insert(
        "text".to_string(),
        match event.get("text") {
            Some(Value::String(text)) => Value::String(text.clone()),
            _ => json!(""),
        },
    );
    row.insert("tool_name".to_string(), optional_string(event, "tool_name"));
    row.insert("model".to_string(), optional_string(event, "model"));
    row.insert("tokens_in".to_string(), optional_number(event, "tokens_in"));
    row.insert(
        "tokens_out".to_string(),
        optional_number(event, "tokens_out"),
    );
    row.insert(
        "extra".to_string(),
        match event.get("extra") {
            Some(extra @ Value::Object(_)) => extra.clone(),
            _ => Value::Object(Map::new()),
        },
    );
    Value::Object(row)
}

/// A conversation row that carries an identity and a timestamp.
pub(crate) fn accepted(event: &Value) -> bool {
    if !event.is_object() {
        return false;
    }
    let Some(event_type) = event.get("event_type").and_then(Value::as_str) else {
        return false;
    };
    if !CONVERSATION_EVENTS.contains(&event_type) {
        return false;
    }
    if !event
        .get("session_id")
        .and_then(Value::as_str)
        .is_some_and(|id| !id.is_empty())
    {
        return false;
    }
    event
        .get("ts")
        .and_then(Value::as_str)
        .is_some_and(|ts| !ts.is_empty())
}

/// The runtime the row states, or the one the partition it came from carries.
pub(crate) fn row_runtime(event: &Value, partition_runtime: &str) -> String {
    match event.get("runtime").and_then(Value::as_str) {
        Some(runtime) if !runtime.is_empty() => runtime.to_string(),
        _ => partition_runtime.to_string(),
    }
}

pub(crate) fn row_order(left: &Value, right: &Value) -> std::cmp::Ordering {
    let text = |row: &Value, key: &str| match row.get(key) {
        Some(value) => js_string(value),
        None => "undefined".to_string(),
    };
    text(left, "ts")
        .cmp(&text(right, "ts"))
        .then_with(|| text(left, "uuid").cmp(&text(right, "uuid")))
}

pub(crate) fn dedupe(rows: Vec<Value>) -> Vec<Value> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut unique = Vec::with_capacity(rows.len());
    for row in rows {
        let uuid = match row.get("uuid") {
            Some(uuid) => js_string(uuid),
            None => "undefined".to_string(),
        };
        if seen.insert(uuid) {
            unique.push(row);
        }
    }
    unique
}

pub(crate) fn render(rows: &[Value]) -> crate::util::Result<String> {
    let mut content = String::new();
    for (index, row) in rows.iter().enumerate() {
        if index > 0 {
            content.push('\n');
        }
        content.push_str(&serde_json::to_string(row)?);
    }
    content.push('\n');
    Ok(content)
}
