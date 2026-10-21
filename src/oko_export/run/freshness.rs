//! Read-only comparison of what Oko's index holds against what the lake
//! cursors recorded, printed as a table and returned as JSON.
//!
//! Oko's index keeps each session's `mtime` and `last_activity` as epoch
//! seconds (the TranscriptIndex+SQL.swift schema); `sqlite3 -readonly` reads
//! their newest values as times, so a live Oko holding the write lock is
//! never disturbed. The lake's cursor store keeps each source file's
//! `mtimeMs`. An index with no session, or a cursor store with no file, has
//! no newest time and says so; a value that is not a time is an error named
//! with the value, never read as the epoch.

use std::fs;
use std::process::Command;

use chrono::{DateTime, Utc};
use serde_json::{json, Value};

use crate::oko_export::oko_support_dir;
use crate::paths::resolve_data_dir;

/// How a time is written in the table and the JSON.
const STAMP: &str = "%Y-%m-%dT%H:%M:%S%.3fZ";

/// The newest times and the session count Oko's index holds.
struct Index {
    sessions: u64,
    mtime: Option<DateTime<Utc>>,
    activity: Option<DateTime<Utc>>,
}

/// One sqlite3 column holding a time as `strftime` wrote it, or nothing for
/// an index with no session.
fn column_time(column: &str) -> Result<Option<DateTime<Utc>>, String> {
    let column = column.trim();
    if column.is_empty() {
        return Ok(None);
    }
    DateTime::parse_from_rfc3339(column)
        .map(|stamp| Some(stamp.with_timezone(&Utc)))
        .map_err(|error| format!("sqlite3 answered {column:?} for a time: {error}"))
}

/// The newest times of Oko's index at `db`, read without taking its lock.
fn read_index(db: &std::path::Path) -> Result<Index, String> {
    let sql = "SELECT COUNT(*), strftime('%Y-%m-%dT%H:%M:%fZ', MAX(mtime), 'unixepoch'), \
               strftime('%Y-%m-%dT%H:%M:%fZ', MAX(COALESCE(last_activity, mtime)), 'unixepoch') FROM sessions;";
    let output = stado_wait::output(
        Command::new("sqlite3").args(["-readonly", "-separator", "|", &db.to_string_lossy(), sql]),
    )
    .map_err(|error| format!("sqlite3 cannot be run: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "sqlite3 refused ({}): {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let columns: Vec<&str> = stdout.trim().split('|').collect();
    let [count, mtime, activity] = columns.as_slice() else {
        return Err(format!("sqlite3 answered {stdout:?}, not a count and two times"));
    };
    Ok(Index {
        sessions: count
            .trim()
            .parse()
            .map_err(|error| format!("sqlite3 answered {count:?} for the session count: {error}"))?,
        mtime: column_time(mtime)?,
        activity: column_time(activity)?,
    })
}

/// Every `mtimeMs` in the cursor store, however deep the store nests it; a
/// value that is no time is named.
fn cursor_times(node: &Value, times: &mut Vec<DateTime<Utc>>) -> Result<(), String> {
    if let Some(ms) = node.get("mtimeMs") {
        let stamp = ms
            .as_f64()
            .and_then(|ms| DateTime::from_timestamp_millis(ms as i64))
            .ok_or_else(|| format!("a cursor records mtimeMs {ms}, which is no time"))?;
        times.push(stamp);
        return Ok(());
    }
    match node {
        Value::Object(map) => map.values().try_for_each(|value| cursor_times(value, times)),
        Value::Array(items) => items.iter().try_for_each(|value| cursor_times(value, times)),
        _ => Ok(()),
    }
}

/// A time as the JSON writes it.
fn stamp(at: Option<DateTime<Utc>>) -> Value {
    match at {
        Some(at) => json!(at.format(STAMP).to_string()),
        None => Value::Null,
    }
}

/// A time and how long ago it was, in whole minutes, for the table.
fn written(at: Option<DateTime<Utc>>, now: DateTime<Utc>) -> String {
    match at {
        Some(at) => format!("{}\t{} min ago", at.format(STAMP), (now - at).num_minutes()),
        None => "none\tnone".to_string(),
    }
}

pub fn freshness() -> Value {
    let now = Utc::now();
    let db_path = oko_support_dir().join("transcript-index.sqlite");
    let db_exists = db_path.exists();
    let (index, oko_error) = match db_exists {
        true => match read_index(&db_path) {
            Ok(index) => (Some(index), None),
            Err(error) => (None, Some(error)),
        },
        false => (None, None),
    };
    let cursors_path = resolve_data_dir(None).join("cursors.json");
    let cursors_exist = cursors_path.exists();
    let mut lake_times = Vec::new();
    let lake_error = match cursors_exist {
        true => fs::read_to_string(&cursors_path)
            .map_err(|error| error.to_string())
            .and_then(|raw| serde_json::from_str::<Value>(&raw).map_err(|error| error.to_string()))
            .and_then(|store| cursor_times(&store, &mut lake_times))
            .err(),
        false => None,
    };
    let lake_mtime = lake_times.iter().max().copied();
    let oko_mtime = index.as_ref().and_then(|index| index.mtime);
    let oko_activity = index.as_ref().and_then(|index| index.activity);
    let fresher = match (oko_mtime, lake_mtime) {
        (Some(oko), Some(lake)) if lake > oko => "lake",
        (Some(oko), Some(lake)) if oko > lake => "oko",
        (Some(_), Some(_)) => "equal",
        (None, Some(_)) => "lake",
        (Some(_), None) => "oko",
        (None, None) => "unknown",
    };
    println!("source\tlatest\tage");
    println!("oko-index (mtime)\t{}", written(oko_mtime, now));
    println!("oko-index (activity)\t{}", written(oko_activity, now));
    println!("lake cursors\t{}", written(lake_mtime, now));
    println!("fresher: {fresher}");
    json!({
        "now": stamp(Some(now)),
        "oko": {
            "db": db_path.to_string_lossy(),
            "exists": db_exists,
            "sessions": index.as_ref().map(|index| index.sessions),
            "maxMtime": stamp(oko_mtime),
            "maxActivity": stamp(oko_activity),
            "error": oko_error,
        },
        "lake": {
            "cursors": cursors_path.to_string_lossy(),
            "exists": cursors_exist,
            "files": lake_times.len(),
            "maxMtime": stamp(lake_mtime),
            "error": lake_error,
        },
        "fresher": fresher,
    })
}
