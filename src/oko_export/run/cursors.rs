//! What the export remembers about each partition, and the rows appended
//! since it last looked.

use std::collections::HashMap;
use std::path::Path;

use serde_json::{json, Map, Value};

use crate::oko_export::partitions::{LineReader, Partition};
use crate::oko_export::row::{
    accepted, export_line, fingerprint, number_value, row_runtime,
};
use crate::oko_export::sessions::IncrementalSession;
use crate::oko_export::{hash_text, read_text, session_key, Tally};
use crate::util::Result;

pub(super) fn read_export_cursors(file: &Path) -> Option<Map<String, Value>> {
    if !file.exists() {
        return None;
    }
    match serde_json::from_str::<Value>(&read_text(file)?) {
        Ok(Value::Object(store)) => Some(store),
        _ => None,
    }
}

pub(super) fn partition_snapshot(partitions: &[Partition]) -> Value {
    let mut snapshot = Map::new();
    for partition in partitions {
        snapshot.insert(
            partition.path.to_string_lossy().into_owned(),
            json!({
                "size": partition.size,
                "mtimeMs": number_value(partition.mtime_ms),
                "physicalSize": partition.physical_size,
            }),
        );
    }
    Value::Object(snapshot)
}

/// The cursor state kept for one partition. An absent key stays absent, which
/// the truncation test distinguishes from a recorded value.
struct ExportCursor {
    size: Option<f64>,
    mtime_ms: Option<f64>,
    physical_size: Option<f64>,
}

fn js_number(value: &Value) -> f64 {
    match value {
        Value::Null => 0.0,
        Value::Bool(flag) => {
            if *flag {
                1.0
            } else {
                0.0
            }
        }
        Value::Number(number) => number.as_f64().unwrap_or(f64::NAN),
        Value::String(text) => {
            let text = text.trim();
            if text.is_empty() {
                0.0
            } else {
                text.parse::<f64>().unwrap_or(f64::NAN)
            }
        }
        _ => f64::NAN,
    }
}

impl ExportCursor {
    fn read(record: &Value) -> Option<Self> {
        let field = |key: &str| record.get(key).map(js_number);
        record.is_object().then(|| Self {
            size: field("size"),
            mtime_ms: field("mtimeMs"),
            physical_size: field("physicalSize"),
        })
    }
}

/// Rows appended since the recorded cursor, or `None` when a partition was
/// truncated or rewritten in place and only a staging rebuild is sound.
pub(super) fn incremental_sessions(
    partitions: &[Partition],
    cursors: &Map<String, Value>,
    tally: &mut Tally,
) -> Result<Option<Vec<IncrementalSession>>> {
    let mut sessions: Vec<IncrementalSession> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for partition in partitions {
        let cursor = cursors
            .get(&partition.path.to_string_lossy().into_owned())
            .and_then(ExportCursor::read);
        let size = partition.size as f64;
        if let Some(cursor) = &cursor {
            let shrank = cursor.size.is_some_and(|recorded| size < recorded);
            let same_size = cursor.size.is_some_and(|recorded| size == recorded);
            let mtime_changed = cursor
                .mtime_ms
                .is_none_or(|recorded| partition.mtime_ms != recorded);
            let rewritten_in_place = match cursor.physical_size {
                None => partition.physical_size == partition.size,
                Some(recorded) => (partition.physical_size as f64) <= recorded,
            };
            if shrank || (same_size && mtime_changed && rewritten_in_place) {
                return Ok(None);
            }
            if same_size {
                continue;
            }
        }
        if partition.size == 0 {
            continue;
        }
        let start = cursor
            .as_ref()
            .and_then(|cursor| cursor.size)
            .filter(|recorded| recorded.is_finite() && *recorded >= 0.0)
            .map(|recorded| recorded as u64)
            .unwrap_or(0);
        let mut reader = LineReader::open(&partition.path, start, partition.size)?;
        while let Some(line) = reader.next_line()? {
            if line.trim().is_empty() {
                continue;
            }
            let event: Value = match serde_json::from_str(&line) {
                Ok(event) => event,
                Err(error) => {
                    tally.malformed += 1;
                    tally.last_error = Some(error.to_string());
                    continue;
                }
            };
            if !accepted(&event) {
                continue;
            }
            let runtime = row_runtime(&event, &partition.runtime);
            let session_id = event["session_id"].as_str().unwrap_or_default().to_string();
            let key = session_key(&runtime, &session_id);
            let slot = match index.get(&key) {
                Some(slot) => *slot,
                None => {
                    let session_hash = hash_text(&key);
                    sessions.push(IncrementalSession {
                        runtime: runtime.clone(),
                        session_hash,
                        rows: Vec::new(),
                    });
                    index.insert(key, sessions.len() - 1);
                    sessions.len() - 1
                }
            };
            let fingerprint = fingerprint(&event, &runtime);
            sessions[slot]
                .rows
                .push(export_line(&event, &runtime, &fingerprint));
        }
    }
    Ok(Some(sessions))
}
