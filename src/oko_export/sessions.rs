//! Per-session output: staging every conversation row to bounded buffers,
//! materializing one session file, merging a delta into an existing one, and
//! pruning files no session claims any more.

use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::partitions::{read_dir_names, LineReader, Partition};
use super::row::{
    accepted, dedupe, export_line, fingerprint, render, row_order, row_runtime,
};
use super::{atomic_write, hash_text, read_text, session_key, Tally};
use crate::util::Result;

const BUFFER_LIMIT: usize = 8388608;

pub(crate) struct StagedSession {
    runtime: String,
    session_hash: String,
    staged_file: PathBuf,
}

fn flush_buffers(buffers: &mut HashMap<PathBuf, String>) -> Result<()> {
    for (file, chunks) in buffers.iter() {
        if let Some(parent) = file.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut handle = OpenOptions::new().create(true).append(true).open(file)?;
        handle.write_all(chunks.as_bytes())?;
    }
    buffers.clear();
    Ok(())
}

/// Spill every conversation row to a per-session staging file, so a rebuild
/// costs one bounded buffer rather than the whole lake in memory.
pub(crate) fn stage_events(
    partitions: &[Partition],
    staging_root: &Path,
    tally: &mut Tally,
) -> Result<Vec<StagedSession>> {
    let mut sessions: Vec<StagedSession> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut buffers: HashMap<PathBuf, String> = HashMap::new();
    let mut buffered_bytes = 0usize;
    for partition in partitions {
        if partition.size == 0 {
            continue;
        }
        let mut reader = LineReader::open(&partition.path, 0, partition.size)?;
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
            let session_hash = hash_text(&key);
            let staged_file = staging_root
                .join(&runtime)
                .join(session_hash.clone() + ".ndjson");
            let fingerprint = fingerprint(&event, &runtime);
            let chunk = serde_json::to_string(&export_line(&event, &runtime, &fingerprint))? + "\n";
            buffered_bytes += chunk.len();
            buffers
                .entry(staged_file.clone())
                .or_default()
                .push_str(&chunk);
            if seen.insert(key) {
                sessions.push(StagedSession {
                    runtime,
                    session_hash,
                    staged_file,
                });
            }
            if buffered_bytes >= BUFFER_LIMIT {
                flush_buffers(&mut buffers)?;
                buffered_bytes = 0;
            }
        }
    }
    flush_buffers(&mut buffers)?;
    Ok(sessions)
}

pub(crate) struct SessionWrite {
    pub(crate) file: PathBuf,
    pub(crate) records: u64,
    pub(crate) changed: bool,
}

fn session_file(output_root: &Path, runtime: &str, session_hash: &str) -> PathBuf {
    output_root
        .join(format!("runtime={runtime}"))
        .join(format!("{session_hash}.jsonl"))
}

pub(crate) fn materialize_session(
    entry: &StagedSession,
    output_root: &Path,
) -> Result<SessionWrite> {
    let staged = read_text(&entry.staged_file).unwrap_or_default();
    let mut rows = Vec::new();
    for line in staged.split('\n') {
        if line.is_empty() {
            continue;
        }
        if let Ok(row) = serde_json::from_str::<Value>(line) {
            rows.push(row);
        }
    }
    let mut rows = dedupe(rows);
    rows.sort_by(row_order);
    let file = session_file(output_root, &entry.runtime, &entry.session_hash);
    let content = render(&rows)?;
    let existing = read_text(&file);
    if existing.as_deref() == Some(content.as_str()) {
        return Ok(SessionWrite {
            file,
            records: rows.len() as u64,
            changed: false,
        });
    }
    atomic_write(&file, &content)?;
    Ok(SessionWrite {
        file,
        records: rows.len() as u64,
        changed: true,
    })
}

pub(crate) fn prune_outputs(output_root: &Path, expected: &HashSet<PathBuf>) -> Result<u64> {
    let mut pruned = 0;
    for runtime_name in read_dir_names(output_root)? {
        if !runtime_name.starts_with("runtime=") {
            continue;
        }
        let runtime_dir = output_root.join(&runtime_name);
        for name in read_dir_names(&runtime_dir)? {
            let file = runtime_dir.join(&name);
            if !name.ends_with(".jsonl") || expected.contains(&file) {
                continue;
            }
            fs::remove_file(&file)?;
            pruned += 1;
        }
    }
    Ok(pruned)
}

pub(crate) struct IncrementalSession {
    pub(crate) runtime: String,
    pub(crate) session_hash: String,
    pub(crate) rows: Vec<Value>,
}

pub(crate) fn merge_incremental_session(
    entry: &IncrementalSession,
    output_root: &Path,
) -> Result<SessionWrite> {
    let file = session_file(output_root, &entry.runtime, &entry.session_hash);
    let existing = read_text(&file);
    let mut rows = Vec::new();
    if let Some(existing) = &existing {
        for line in existing.split('\n') {
            if line.is_empty() {
                continue;
            }
            // A torn derived file is repaired from the valid rows plus the delta.
            if let Ok(row) = serde_json::from_str::<Value>(line) {
                rows.push(row);
            }
        }
    }
    rows.extend(entry.rows.iter().cloned());
    let mut unique = dedupe(rows);
    unique.sort_by(row_order);
    let content = render(&unique)?;
    let records = entry.rows.len() as u64;
    if existing.as_deref() == Some(content.as_str()) {
        return Ok(SessionWrite {
            file,
            records,
            changed: false,
        });
    }
    atomic_write(&file, &content)?;
    Ok(SessionWrite {
        file,
        records,
        changed: true,
    })
}
