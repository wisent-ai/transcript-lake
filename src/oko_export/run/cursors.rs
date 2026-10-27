//! What the export remembers about each partition, and the rows appended
//! since it last looked.

use std::collections::HashMap;
use std::path::Path;

use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::oko_export::partitions::{LineReader, Partition};
use crate::oko_export::row::{accepted, export_line, fingerprint, number_value, row_runtime};
use crate::oko_export::sessions::IncrementalSession;
use crate::oko_export::{hash_text, read_text, session_key, Tally};
use crate::util::Result;

/// The recorded cursors; `None` when there is no store or it does not parse,
/// which a full export rebuilds. A store that cannot be read is the error.
pub(super) fn read_export_cursors(file: &Path) -> Result<Option<Map<String, Value>>> {
    let Some(text) = read_text(file)? else {
        return Ok(None);
    };
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(store)) => Ok(Some(store)),
        _ => Ok(None),
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

/// The cursor kept for one partition, read in the shape `partition_snapshot`
/// writes it: integer sizes and the modification time as a number, or null
/// when the clock gave none.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ExportCursor {
    size: u64,
    mtime_ms: Option<f64>,
    physical_size: u64,
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
        let cursor = match cursors.get(&partition.path.to_string_lossy().into_owned()) {
            None => None,
            // A cursor not in the shape this export writes cannot say where
            // the partition was read to: only a full rebuild is sound.
            Some(record) => match serde_json::from_value::<ExportCursor>(record.clone()) {
                Ok(cursor) => Some(cursor),
                Err(_unreadable) => return Ok(None),
            },
        };
        if let Some(cursor) = &cursor {
            let shrank = partition.size < cursor.size;
            let same_size = partition.size == cursor.size;
            let mtime_changed = cursor.mtime_ms != Some(partition.mtime_ms);
            let rewritten_in_place = partition.physical_size <= cursor.physical_size;
            if shrank || (same_size && mtime_changed && rewritten_in_place) {
                return Ok(None);
            }
            if same_size {
                continue;
            }
        }
        let start = cursor.as_ref().map(|cursor| cursor.size);
        let mut reader = LineReader::open(&partition.path, start, partition.size)?;
        while let Some(line) = reader.next_line()? {
            if line.trim().is_empty() {
                continue;
            }
            let event: Value = match serde_json::from_str(&line) {
                Ok(event) => event,
                Err(error) => {
                    tally.malformed.push(error.to_string());
                    continue;
                }
            };
            if !accepted(&event) {
                continue;
            }
            let runtime = row_runtime(&event, &partition.runtime);
            // `accepted` holds every row it lets through to a text session id.
            let Some(session_id) = event["session_id"].as_str() else {
                continue;
            };
            let key = session_key(&runtime, &session_id);
            let slot = match index.get(&key) {
                Some(slot) => *slot,
                None => {
                    // The new session's slot is the length before it is pushed.
                    let slot = sessions.len();
                    sessions.push(IncrementalSession {
                        runtime: runtime.clone(),
                        session_hash: hash_text(&key),
                        rows: Vec::new(),
                    });
                    index.insert(key, slot);
                    slot
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
