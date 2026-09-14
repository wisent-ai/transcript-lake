//! One export run: full rebuild, incremental merge, cursor publication, and
//! the optional Oko reindex performed while the writer lease is still held.

mod cursors;
pub(crate) mod freshness;

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use serde_json::{json, Map, Value};

use self::cursors::{incremental_sessions, partition_snapshot, read_export_cursors};
use super::partitions::{event_partition_files, Partition};
use super::row::{accepted, export_line, fingerprint, row_runtime};
use super::sessions::{
    materialize_session, merge_incremental_session, prune_outputs, stage_events,
    IncrementalSession,
};
use super::{atomic_write, hash_text, remove_tree, session_key, Tally};
use crate::cursors::open_writer_lease;
use crate::util::{Error, Result};

/// An incremental merge touches only the sessions whose rows it read, so it
/// removes no session file; a full rebuild is the pass that can prune one.
const INCREMENTAL_PRUNED: u64 = 0;

/// Apply already-masked canonical rows directly to Oko's per-session view.
///
/// The real-time stream calls this before advancing a source cursor, so Oko
/// never waits for a second partition scan. Recovery export remains available
/// to reconstruct the projection from authoritative Lake partitions.
pub(crate) fn project_events(data_dir: &Path, events: Vec<Value>) -> Result<()> {
    let output_root = data_dir.join("exports").join("oko");
    let mut sessions: Vec<IncrementalSession> = Vec::new();
    let mut index: HashMap<String, usize> = HashMap::new();
    for event in events {
        if !accepted(&event) {
            continue;
        }
        let runtime = row_runtime(&event, "");
        if runtime.is_empty() || runtime == crate::types::HOOKS {
            continue;
        }
        let session_id = event["session_id"].as_str().unwrap_or_default();
        let key = session_key(&runtime, session_id);
        let slot = match index.get(&key) {
            Some(slot) => *slot,
            None => {
                sessions.push(IncrementalSession {
                    runtime: runtime.clone(),
                    session_hash: hash_text(&key),
                    rows: Vec::new(),
                });
                index.insert(key, sessions.len() - 1);
                sessions.len() - 1
            }
        };
        let event_id = fingerprint(&event, &runtime);
        sessions[slot]
            .rows
            .push(export_line(&event, &runtime, &event_id));
    }
    for session in &sessions {
        merge_incremental_session(session, &output_root)?;
    }
    Ok(())
}

struct ExportResult {
    sessions: usize,
    records: u64,
    written: u64,
    unchanged: u64,
    pruned: u64,
    mode: &'static str,
}

fn full_export(
    partitions: &[Partition],
    output_root: &Path,
    staging_root: &Path,
    tally: &mut Tally,
) -> Result<ExportResult> {
    remove_tree(staging_root)?;
    fs::create_dir_all(staging_root)?;
    let sessions = stage_events(partitions, staging_root, tally)?;
    if tally.malformed > 0 {
        remove_tree(staging_root)?;
        return Err(Error(
            "full Oko export refused malformed Lake rows; authoritative partitions were not modified"
                .to_string(),
        ));
    }
    let mut expected: HashSet<PathBuf> = HashSet::new();
    let mut written = 0;
    let mut unchanged = 0;
    let mut records = 0;
    for entry in &sessions {
        let result = materialize_session(entry, output_root)?;
        expected.insert(result.file);
        records += result.records;
        if result.changed {
            written += 1;
        } else {
            unchanged += 1;
        }
    }
    let pruned = prune_outputs(output_root, &expected)?;
    remove_tree(staging_root)?;
    Ok(ExportResult {
        sessions: sessions.len(),
        records,
        written,
        unchanged,
        pruned,
        mode: "full",
    })
}

fn export_oko_locked(full: bool, reindex: bool, data_dir: &Path) -> Result<Value> {
    let started_at = Instant::now();
    let output_root = data_dir.join("exports").join("oko");
    let staging_root = data_dir.join("staging").join("oko-export");
    let cursor_file = output_root.join("export-cursors.json");
    let mut tally = Tally::default();
    let partitions = event_partition_files(data_dir)?;
    let cursors = if full {
        None
    } else {
        read_export_cursors(&cursor_file)
    };
    let result = match &cursors {
        None => full_export(&partitions, &output_root, &staging_root, &mut tally)?,
        Some(cursors) => match incremental_sessions(&partitions, cursors, &mut tally)? {
            None => full_export(&partitions, &output_root, &staging_root, &mut tally)?,
            Some(sessions) => {
                if tally.malformed > 0 {
                    return Err(Error(
                            "incremental Oko export refused malformed Lake rows; export cursor was not advanced"
                                .to_string(),
                        ));
                }
                let mut records = 0;
                let mut written = 0;
                let mut unchanged = 0;
                for entry in &sessions {
                    let merged = merge_incremental_session(entry, &output_root)?;
                    records += merged.records;
                    if merged.changed {
                        written += 1;
                    } else {
                        unchanged += 1;
                    }
                }
                ExportResult {
                    sessions: sessions.len(),
                    records,
                    written,
                    unchanged,
                    pruned: INCREMENTAL_PRUNED,
                    mode: "incremental",
                }
            }
        },
    };
    atomic_write(
        &cursor_file,
        &(pretty_one_space(&partition_snapshot(&partitions))? + "\n"),
    )?;
    let mut summary = Map::new();
    summary.insert(
        "outputRoot".to_string(),
        json!(output_root.to_string_lossy()),
    );
    summary.insert("sessions".to_string(), json!(result.sessions));
    summary.insert("records".to_string(), json!(result.records));
    summary.insert("written".to_string(), json!(result.written));
    summary.insert("unchanged".to_string(), json!(result.unchanged));
    summary.insert("pruned".to_string(), json!(result.pruned));
    summary.insert("mode".to_string(), json!(result.mode));
    summary.insert("malformed".to_string(), json!(tally.malformed));
    summary.insert(
        "durationMs".to_string(),
        json!(started_at.elapsed().as_millis() as u64),
    );
    if let Some(last_error) = tally.last_error {
        summary.insert("lastError".to_string(), json!(last_error));
    }
    if reindex {
        summary.insert("reindex".to_string(), run_reindex());
    }
    Ok(Value::Object(summary))
}

/// The export cursor file is published with one-space indentation, exactly as
/// the previous implementation wrote it.
fn pretty_one_space(value: &Value) -> Result<String> {
    let mut buffer = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(b" ");
    let mut serializer = serde_json::Serializer::with_formatter(&mut buffer, formatter);
    serde::Serialize::serialize(value, &mut serializer)?;
    Ok(String::from_utf8_lossy(&buffer).into_owned())
}

/// The same export, optionally followed by an Oko reindex while the lease is
/// still held, so no concurrent writer mutates the lake mid-reindex.
pub fn export_oko_with_reindex(full: bool, reindex: bool, data_dir: &Path) -> Result<Value> {
    let _lease = open_writer_lease(data_dir)?;
    export_oko_locked(full, reindex, data_dir)
}

// A flagless reindex discovers the Lake export root and remains incremental:
// unchanged per-session files keep their mtimes, so Oko skips them.
fn run_reindex() -> Value {
    let command = std::env::var("OKO_CLI")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "oko-cli".to_string());
    let args = ["transcripts", "reindex", "--json"];
    match Command::new(&command).args(args).output() {
        Err(error) => json!({
            "ran": false,
            "command": format!("{command} {}", args.join(" ")),
            "error": error.to_string(),
        }),
        Ok(output) => json!({
            "ran": true,
            "status": output.status.code(),
            "output": String::from_utf8_lossy(&output.stdout).trim(),
        }),
    }
}
