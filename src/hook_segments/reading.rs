//! Reading what a hook wrote: the JSON shapes the record mapping accepts, the
//! JavaScript conversions it inherits, and the durable write every published
//! file goes through so a reader never sees a half-written one.

use std::fs::{self, File};
use std::io::Write;
use std::path::Path;

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::types::RawEvent;
use crate::util::Result;

use super::*;

pub(super) fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

pub(super) fn segment_name(id: &str) -> String {
    format!("segment-{id}.jsonl")
}

pub(super) fn ack_name(id: &str) -> String {
    format!("segment-{id}.ack.json")
}

/// A JSON object, or nothing. Arrays and scalars are not records here.
pub(super) fn parse(text: &str) -> Option<Value> {
    let value = serde_json::from_str::<Value>(text).ok()?;
    value.is_object().then_some(value)
}

pub(super) fn string_field(value: &Value, key: &str) -> Option<String> {
    match value.get(key) {
        Some(Value::String(text)) if !text.is_empty() => Some(text.clone()),
        _ => None,
    }
}

/// JS truthiness, for the `x || null` fallbacks the record mapping is built from.
pub(super) fn truthy(value: &Value) -> bool {
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

/// `record[key] || null`.
pub(super) fn or_null(record: &Value, key: &str) -> Value {
    match record.get(key) {
        Some(value) if truthy(value) => value.clone(),
        _ => Value::Null,
    }
}

/// `record[key] ?? null`.
pub(super) fn nullish(record: &Value, key: &str) -> Value {
    match record.get(key) {
        Some(value) if !value.is_null() => value.clone(),
        _ => Value::Null,
    }
}

/// JS `Number(value)`, which the record mapping applies to the raw `ts` field.
pub(super) fn js_number(value: Option<&Value>) -> f64 {
    match value {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(flag)) => {
            if *flag {
                1.0
            } else {
                0.0
            }
        }
        Some(Value::Number(number)) => number.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(text)) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                0.0
            } else {
                trimmed.parse::<f64>().unwrap_or(f64::NAN)
            }
        }
        Some(Value::Array(items)) => match items.len() {
            0 => 0.0,
            1 => js_number(items.first()),
            _ => f64::NAN,
        },
        Some(Value::Object(_)) => f64::NAN,
    }
}

pub(super) fn epoch_iso(millis: f64) -> Option<String> {
    if !millis.is_finite() {
        return None;
    }
    let stamp = chrono::DateTime::from_timestamp_millis(millis as i64)?;
    Some(stamp.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
}

/// One record inside a closed segment, as the canonical event the sink will mask,
/// canonicalize and write. Every `extra` key is always present, including the nulls,
/// because sql/views.sql and sql/signals.sql project them by name.
pub(super) fn map_canonical_hook_record(
    record: &Value,
    segment_id: &str,
    segment_created_at: &Value,
    sequence: usize,
) -> RawEvent {
    let ts = epoch_iso(js_number(record.get("ts")));
    let mut extra = Map::new();
    extra.insert("source_type".into(), or_null(record, "type"));
    extra.insert("hook_id".into(), or_null(record, "id"));
    extra.insert("decision".into(), or_null(record, "decision"));
    extra.insert("code".into(), nullish(record, "code"));
    extra.insert(
        "timed_out".into(),
        Value::Bool(record.get("timedOut") == Some(&Value::Bool(true))),
    );
    extra.insert("infra".into(), or_null(record, "infra"));
    extra.insert("source".into(), or_null(record, "source"));
    extra.insert("episode_id".into(), or_null(record, "episode_id"));
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
    extra.insert("label".into(), or_null(record, "label"));
    extra.insert("repair_kind".into(), or_null(record, "kind"));
    extra.insert("evidence".into(), or_null(record, "evidence"));
    let text = string_field(record, "reason")
        .or_else(|| string_field(record, "text"))
        .unwrap_or_default();
    RawEvent {
        ts,
        session_id: string_field(record, "session_id"),
        project: string_field(record, "project"),
        event_type: "hook_decision".to_string(),
        text,
        tool_name: string_field(record, "id"),
        model: None,
        tokens_in: None,
        tokens_out: None,
        extra,
    }
}

pub(super) fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)?.sync_all()?;
    Ok(())
}

pub(super) fn durable_write(path: &Path, content: &[u8]) -> Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let name = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let temporary = parent.join(format!(".{name}.{}.tmp", uuid::Uuid::new_v4()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    file.write_all(content)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temporary, path)?;
    sync_directory(parent)?;
    Ok(())
}

