//! Read-only comparison of what Oko's index holds against what the lake
//! cursors recorded, printed as a table and returned as JSON.

use std::fs;
use std::process::Command;

use serde_json::{json, Value};

use crate::oko_export::oko_support_dir;
use crate::oko_export::row::number_value;
use crate::paths::resolve_data_dir;

const SECOND_MS: f64 = 1000.0;
const MINUTE_MS: f64 = 60000.0;
const MINUTES_PER_HOUR: f64 = 60.0;
const HOURS_PER_DAY: f64 = 24.0;
const CURSOR_WALK_DEPTH: usize = 4;
const PAD: usize = 26;

fn iso_or_na(ms: Option<f64>) -> String {
    let Some(ms) = ms else {
        return "n/a".to_string();
    };
    chrono::DateTime::from_timestamp_millis(ms as i64)
        .map(|stamp| stamp.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
        .unwrap_or_else(|| "n/a".to_string())
}

fn js_round(value: f64) -> f64 {
    (value + 0.5).floor()
}

fn age_label(ms: Option<f64>, now_ms: f64) -> String {
    let Some(ms) = ms else {
        return "n/a".to_string();
    };
    let minutes = js_round((now_ms - ms) / MINUTE_MS);
    if minutes < MINUTES_PER_HOUR {
        return format!("{minutes}m");
    }
    let hours = js_round(minutes / MINUTES_PER_HOUR);
    if hours < HOURS_PER_DAY {
        return format!("{hours}h");
    }
    format!("{}d", js_round(hours / HOURS_PER_DAY))
}

fn walk_cursor_times(node: &Value, depth: usize, files: &mut u64, newest: &mut Option<f64>) {
    if depth > CURSOR_WALK_DEPTH || !(node.is_object() || node.is_array()) {
        return;
    }
    if let Some(ms) = node.get("mtimeMs").and_then(Value::as_f64) {
        if ms.is_finite() {
            *files += 1;
            if newest.is_none_or(|current| ms > current) {
                *newest = Some(ms);
            }
            return;
        }
    }
    match node {
        Value::Object(map) => {
            for value in map.values() {
                walk_cursor_times(value, depth + 1, files, newest);
            }
        }
        Value::Array(items) => {
            for value in items {
                walk_cursor_times(value, depth + 1, files, newest);
            }
        }
        _ => {}
    }
}

fn now_ms() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|delta| delta.as_secs() as f64 * SECOND_MS + f64::from(delta.subsec_nanos()) / 1e6)
        .unwrap_or(0.0)
        .floor()
}

/// `Number(text)` for one sqlite3 column: an absent column stays absent, an
/// empty one is zero, exactly as the previous implementation read them.
fn column_number(column: Option<&str>) -> Option<f64> {
    let column = column?.trim();
    if column.is_empty() {
        return Some(0.0);
    }
    column.parse::<f64>().ok().filter(|value| value.is_finite())
}

// Read-only freshness comparison: Oko's index (sessions.mtime / last_activity are
// epoch seconds, see the TranscriptIndex+SQL.swift schema) versus the lake's cursor
// checkpoints (mtimeMs per source file). Queried via `sqlite3 -readonly` so a live
// Oko holding the write lock is never disturbed.
pub fn freshness() -> Value {
    let now = now_ms();
    let db_path = oko_support_dir().join("transcript-index.sqlite");
    let db_exists = db_path.exists();
    let mut oko_sessions: Option<f64> = None;
    let mut oko_mtime: Option<f64> = None;
    let mut oko_activity: Option<f64> = None;
    let mut oko_error: Option<String> = None;
    if db_exists {
        let sql = "SELECT MAX(mtime), MAX(COALESCE(last_activity, mtime)), COUNT(*) FROM sessions;";
        let run = Command::new("sqlite3")
            .args([
                "-readonly",
                "-separator",
                "|",
                &db_path.to_string_lossy(),
                sql,
            ])
            .output();
        match run {
            Err(error) => oko_error = Some(error.to_string()),
            Ok(output) if output.status.code() != Some(0) => {
                oko_error = Some(String::from_utf8_lossy(&output.stderr).trim().to_string());
            }
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let mut columns = stdout.trim().split('|');
                let mtime = column_number(columns.next());
                let activity = column_number(columns.next());
                let count = column_number(columns.next());
                if let Some(mtime) = mtime {
                    oko_mtime = Some(mtime * SECOND_MS);
                }
                if let Some(activity) = activity {
                    oko_activity = Some(activity * SECOND_MS);
                }
                oko_sessions = count;
            }
        }
    }
    let cursors_path = resolve_data_dir(None).join("cursors.json");
    let cursors_exist = cursors_path.exists();
    let mut lake_files = 0u64;
    let mut lake_mtime: Option<f64> = None;
    let mut lake_error: Option<String> = None;
    if cursors_exist {
        // Corrupt cursors mean "no recency signal", which the report states openly.
        match fs::read_to_string(&cursors_path)
            .map_err(|error| error.to_string())
            .and_then(|raw| serde_json::from_str::<Value>(&raw).map_err(|error| error.to_string()))
        {
            Ok(store) => walk_cursor_times(&store, 0, &mut lake_files, &mut lake_mtime),
            Err(error) => lake_error = Some(error),
        }
    }
    let fresher = match (oko_mtime, lake_mtime) {
        (Some(oko), Some(lake)) => {
            if lake > oko {
                "lake"
            } else if oko > lake {
                "oko"
            } else {
                "equal"
            }
        }
        (None, Some(_)) => "lake",
        (Some(_), None) => "oko",
        (None, None) => "unknown",
    };
    println!("{:<PAD$}{:<PAD$}{}", "source", "latest", "age");
    println!(
        "{:<PAD$}{:<PAD$}{}",
        "oko-index (mtime)",
        iso_or_na(oko_mtime),
        age_label(oko_mtime, now)
    );
    println!(
        "{:<PAD$}{:<PAD$}{}",
        "oko-index (activity)",
        iso_or_na(oko_activity),
        age_label(oko_activity, now)
    );
    println!(
        "{:<PAD$}{:<PAD$}{}",
        "lake cursors",
        iso_or_na(lake_mtime),
        age_label(lake_mtime, now)
    );
    println!("fresher: {fresher}");
    json!({
        "now": iso_or_na(Some(now)),
        "oko": {
            "db": db_path.to_string_lossy(),
            "exists": db_exists,
            "sessions": oko_sessions.map_or(Value::Null, number_value),
            "maxMtimeMs": oko_mtime.map_or(Value::Null, number_value),
            "maxActivityMs": oko_activity.map_or(Value::Null, number_value),
            "error": oko_error,
        },
        "lake": {
            "cursors": cursors_path.to_string_lossy(),
            "exists": cursors_exist,
            "files": lake_files,
            "maxMtimeMs": lake_mtime.map_or(Value::Null, number_value),
            "error": lake_error,
        },
        "fresher": fresher,
    })
}
