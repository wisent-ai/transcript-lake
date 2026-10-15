//! The health of the cursor store and of the live stream, read for `status`
//! and `doctor` without changing either.
use std::fs;
use std::path::Path;

use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Serialize)]
pub struct CursorStatus {
    pub state: &'static str,
    /// Records in the cursor store; absent when no store could be read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub files: Option<usize>,
    #[serde(rename = "newestSourceMtime")]
    pub newest_source_mtime: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Cursor inventory for `status` and `doctor`; never mutates the store.
pub fn read_cursor_status(path: &Path) -> CursorStatus {
    if !path.exists() {
        return CursorStatus {
            state: "absent",
            files: None,
            newest_source_mtime: None,
            error: None,
        };
    }
    let invalid = |error: String| CursorStatus {
        state: "invalid",
        files: None,
        newest_source_mtime: None,
        error: Some(error),
    };
    let raw = match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) => return invalid(error.to_string()),
    };
    let parsed: Value = match serde_json::from_str(&raw) {
        Ok(parsed) => parsed,
        Err(error) => return invalid(error.to_string()),
    };
    let Some(store) = parsed.as_object() else {
        return invalid("cursor store is not an object".to_string());
    };
    let newest = store
        .values()
        .filter_map(|record| record.get("mtimeMs").and_then(Value::as_f64))
        .filter(|value| value.is_finite())
        .fold(f64::NEG_INFINITY, f64::max);
    // No finite stamp is no newest time; one outside the calendar is none too.
    let newest_source_mtime = crate::adapters::common::iso_epoch_millis(newest);
    CursorStatus {
        state: "healthy",
        files: Some(store.len()),
        newest_source_mtime,
        error: None,
    }
}

#[derive(Debug, Serialize)]
pub struct StreamStatus {
    pub state: &'static str,
    pub summary: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub fn read_stream_status(path: &Path) -> StreamStatus {
    if !path.exists() {
        return StreamStatus {
            state: "absent",
            summary: None,
            error: None,
        };
    }
    match fs::read_to_string(path)
        .map_err(|e| e.to_string())
        .and_then(|raw| serde_json::from_str::<Value>(&raw).map_err(|error| error.to_string()))
    {
        Ok(summary) => StreamStatus {
            state: "healthy",
            summary: Some(summary),
            error: None,
        },
        Err(error) => StreamStatus {
            state: "invalid",
            summary: None,
            error: Some(error),
        },
    }
}
