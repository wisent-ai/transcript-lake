//! Reading what a hook wrote: the JSON shapes the record mapping accepts and
//! the canonical event one closed-segment record becomes. Publishing a file
//! goes through the Lake's one durable publish.

use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::types::RawEvent;

pub(super) use crate::lake::durable::sync_directory;

pub(super) fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub(super) fn segment_name(id: &str) -> String {
    format!("segment-{id}.jsonl")
}

pub(super) fn ack_name(id: &str) -> String {
    format!("segment-{id}.ack.json")
}

/// A JSON object, or why the text is not one. Arrays and scalars are not
/// records here.
pub(super) fn parse(text: &str) -> std::result::Result<Value, String> {
    let value =
        serde_json::from_str::<Value>(text).map_err(|error| format!("is not JSON: {error}"))?;
    if value.is_object() {
        Ok(value)
    } else {
        Err(format!("is not a JSON object but {value}"))
    }
}

pub(super) fn string_field(value: &Value, key: &str) -> Option<String> {
    match value.get(key) {
        Some(Value::String(text)) if !text.is_empty() => Some(text.clone()),
        _ => None,
    }
}

/// `record[key]` when the hook recorded something there: an absent field,
/// null and an empty text record nothing, and are null.
pub(super) fn recorded(record: &Value, key: &str) -> Value {
    match record.get(key) {
        None | Some(Value::Null) => Value::Null,
        Some(Value::String(text)) if text.is_empty() => Value::Null,
        Some(value) => value.clone(),
    }
}

/// `record[key]` as recorded, null when absent.
pub(super) fn nullish(record: &Value, key: &str) -> Value {
    match record.get(key) {
        Some(value) => value.clone(),
        None => Value::Null,
    }
}

/// An ISO time for `millis` since the epoch, or none when no time has that
/// many milliseconds.
pub(super) fn epoch_iso(millis: f64) -> Option<String> {
    if !millis.is_finite() {
        return None;
    }
    let stamp = chrono::DateTime::from_timestamp_millis(millis as i64)?;
    Some(stamp.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
}

/// The record's `ts`: milliseconds since the epoch as a JSON number or a
/// numeric text, as an ISO time. A record without a `ts` has no time; one
/// whose `ts` is anything else says why it is not a time.
pub(super) fn record_time(value: Option<&Value>) -> std::result::Result<Option<String>, String> {
    let millis = match value {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::Number(number)) => number
            .as_f64()
            .ok_or_else(|| format!("ts {number} is not a number of milliseconds"))?,
        Some(Value::String(text)) => text
            .trim()
            .parse::<f64>()
            .map_err(|error| format!("ts {text:?} is not a number of milliseconds: {error}"))?,
        Some(other) => return Err(format!("ts {other} is neither a number nor a numeric text")),
    };
    epoch_iso(millis)
        .map(Some)
        .ok_or_else(|| format!("ts {millis} is no representable time"))
}

/// One record inside a closed segment, as the canonical event the sink will mask,
/// canonicalize and write. Every `extra` key is always present, including the nulls,
/// because sql/views.sql and sql/signals.sql project them by name. A `ts` that is
/// not a time leaves the event without one and says why in `ts_error`.
pub(super) fn map_canonical_hook_record(
    record: &Value,
    segment_id: &str,
    segment_created_at: &Value,
    sequence: usize,
) -> RawEvent {
    let (ts, ts_error) = match record_time(record.get("ts")) {
        Ok(ts) => (ts, Value::Null),
        Err(cause) => (None, Value::String(cause)),
    };
    let mut extra = Map::new();
    extra.insert("source_type".into(), recorded(record, "type"));
    extra.insert("hook_id".into(), recorded(record, "id"));
    extra.insert("decision".into(), recorded(record, "decision"));
    extra.insert("code".into(), nullish(record, "code"));
    extra.insert(
        "timed_out".into(),
        Value::Bool(record.get("timedOut") == Some(&Value::Bool(true))),
    );
    extra.insert("infra".into(), recorded(record, "infra"));
    extra.insert("source".into(), recorded(record, "source"));
    extra.insert("episode_id".into(), recorded(record, "episode_id"));
    extra.insert(
        "adaptive_state_persisted".into(),
        Value::Bool(record.get("adaptiveStatePersisted") == Some(&Value::Bool(true))),
    );
    extra.insert(
        "causal_episode_persisted".into(),
        Value::Bool(record.get("causalEpisodePersisted") == Some(&Value::Bool(true))),
    );
    extra.insert("segment_created_at".into(), segment_created_at.clone());
    extra.insert("segment_id".into(), Value::from(segment_id));
    extra.insert("sequence".into(), Value::from(sequence));
    extra.insert("payload_ts".into(), nullish(record, "payloadTs"));
    extra.insert("payload".into(), nullish(record, "payload"));
    extra.insert("meta".into(), nullish(record, "meta"));
    extra.insert("label".into(), recorded(record, "label"));
    extra.insert("repair_kind".into(), recorded(record, "kind"));
    extra.insert("evidence".into(), recorded(record, "evidence"));
    extra.insert("ts_error".into(), ts_error);
    RawEvent {
        ts,
        session_id: string_field(record, "session_id"),
        project: string_field(record, "project"),
        event_type: "hook_decision".to_string(),
        text: string_field(record, "reason").or_else(|| string_field(record, "text")),
        tool_name: string_field(record, "id"),
        model: None,
        tokens_in: None,
        tokens_out: None,
        extra,
    }
}
