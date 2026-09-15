//! Source ingestion validates its consumed prefix before resuming a cursor.
//! Rewrites and legacy cursors recover through the same masked canonical writer.
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::Serialize;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::cursors::{open_writer_lease, CursorRecord, Cursors};
use crate::util::{machine_name, Error, Result};

mod file;
mod live;
mod replay;
mod source;
mod writer;
use file::stream_file;
pub use live::{catch_up, stream_paths};
pub use replay::replay;
use writer::Writer;

/// Text longer than this many UTF-16 units is cut, in `text` and in every
/// string inside `extra`.
const TEXT_CAP: usize = 65536;
/// Events buffered before a partition append and a cursor checkpoint.
const BATCH_EVENTS: usize = 512;
/// How deep masking descends into `extra` before a value becomes null.
const EXTRA_DEPTH: i32 = 4;
const PART_DIGEST_LEN: usize = 12;
const READ_BUFFER: usize = 64 * 1024;

/// Parameters for an explicit recovery replay into an empty Lake.
pub struct ReplayOptions {
    pub source: Option<String>,
    pub data_dir: PathBuf,
}

/// Per-runtime counters, serialized in the order the previous implementation
/// emitted them.
#[derive(Debug, Default, Serialize)]
struct Tally {
    files: u64,
    events: u64,
    #[serde(rename = "maskedHits")]
    masked_hits: u64,
    skipped: u64,
    #[serde(rename = "replayedFiles")]
    replayed: u64,
    failures: u64,
}

/// One operator-visible streaming warning. The process remains alive so the
/// next source notification can retry the same uncommitted bytes.
pub fn warn(message: &str) {
    eprintln!("stream: {message}");
}

fn hex_digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// File name without its last extension, matching the previous
/// implementation's `basename(file).replace(/\.[^.]+$/, '')`: a trailing dot
/// is not an extension, and a leading-dot name is all extension.
fn file_stem(name: &str) -> String {
    match name.rfind('.') {
        Some(index) if index + 1 < name.len() => name[..index].to_string(),
        _ => name.to_string(),
    }
}

fn total_hits(counts: &crate::redact::MaskCounts) -> u64 {
    counts.token + counts.entropy + counts.assignment
}

/// Ingest exactly one validated transcript root through the same writer,
/// cursor, masking, and Oko projection boundary used by the live stream.
pub fn ingest_source(data_dir: &Path, runtime: &str, root: &Path) -> Result<Value> {
    let mut lease = open_writer_lease(data_dir)?;
    let summary = ingest_source_locked(data_dir, runtime, root);
    lease.close();
    summary
}

fn ingest_source_locked(data_dir: &Path, runtime: &str, root: &Path) -> Result<Value> {
    let started = Instant::now();
    let adapter = crate::adapters::by_name(runtime)
        .ok_or_else(|| Error(format!("transcript adapter unavailable: {runtime}")))?;
    let entries = adapter.list_sessions(root);
    let discovered = entries.len() as u64;
    let mut cursors = Cursors::open(data_dir)?;
    let mut writer = Writer::new(data_dir.to_path_buf(), machine_name());
    let before = total_hits(&writer.masker.counts());
    let mut tally = Tally::default();
    for entry in entries {
        let meta = fs::metadata(&entry.file)
            .map_err(|error| Error(format!("stat failed for {}: {error}", entry.file.display())))?;
        let key = entry.file.to_string_lossy().to_string();
        if let Some(CursorRecord::Bytes(cursor)) = cursors.get(&key)? {
            if cursor.is_current(&meta) {
                tally.skipped += 1;
                continue;
            }
        }
        stream_file(
            &mut writer,
            &mut cursors,
            adapter.as_ref(),
            &entry,
            false,
            &mut tally,
        )
        .map_err(|error| Error(format!("{}: {error}", entry.file.display())))?;
        tally.files += 1;
    }
    tally.masked_hits = total_hits(&writer.masker.counts()) - before;
    cursors.flush()?;
    let mut per_runtime = Map::new();
    per_runtime.insert(runtime.to_string(), serde_json::to_value(&tally)?);
    Ok(json!({
        "perRuntime": Value::Object(per_runtime),
        "maskCounts": writer.masker.counts(),
        "durationMs": started.elapsed().as_millis() as u64,
        "filesDiscovered": discovered,
        "filesStreamed": tally.files,
        "filesImported": tally.files,
        "filesUnchanged": tally.skipped,
        "filesReplayed": tally.replayed,
        "eventsImported": tally.events,
        "partial": false,
        "failures": 0,
    }))
}
