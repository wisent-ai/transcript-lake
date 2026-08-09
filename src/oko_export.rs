//! Materialize every masked conversation runtime as canonical per-session JSONL
//! under LAKE_DATA/exports/oko. Oko imports this stable view and no longer
//! parses vendor transcript stores for its catalog, search, or statistics.
//!
//! Normal runs track each append-only partition by size and mtime, merge only
//! new rows into affected sessions, deduplicate by a deterministic event UUID,
//! and preserve unchanged file mtimes. A first run, explicit --full, partition
//! truncation, or same-size rewrite rebuilds from all Lake partitions through
//! bounded staging buffers. Session writes and cursor publication are atomic.
use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::cursors::open_writer_lease;
use crate::paths::resolve_data_dir;
use crate::util::{home_dir, mtime_ms, Error, Result};

const FP_LEN: usize = 32;
const BUFFER_LIMIT: usize = 8388608;
const READ_CHUNK: usize = 65536;
const SECOND_MS: f64 = 1000.0;
const MINUTE_MS: f64 = 60000.0;
const MINUTES_PER_HOUR: f64 = 60.0;
const HOURS_PER_DAY: f64 = 24.0;
const CURSOR_WALK_DEPTH: usize = 4;
const PAD: usize = 26;
const CONVERSATION_EVENTS: [&str; 6] =
    ["user", "assistant", "thinking", "tool_call", "tool_result", "meta"];

fn oko_support_dir() -> PathBuf {
    home_dir().join("Library").join("Application Support").join("Oko")
}

/// The Oko transcript index this machine would read. Shared with the DuckDB
/// bridge, which has to substitute it into the signal views because ATTACH
/// takes a literal path and does not expand a tilde.
pub fn oko_index_path() -> PathBuf {
    oko_support_dir().join("transcript-index.sqlite")
}

/// Directory entries, treating a missing or non-directory path as empty.
/// Sorted by name: `readdirSync` returns strcmp order, and the export walk is
/// observable in the file Oko reads, so the order is part of the contract.
fn read_dir_names(dir: &Path) -> Result<Vec<String>> {
    match fs::read_dir(dir) {
        Ok(entries) => {
            let mut names: Vec<String> = entries
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect();
            names.sort_unstable_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
            Ok(names)
        }
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(Vec::new())
        }
        Err(error) => Err(error.into()),
    }
}

/// Offset just past the last complete line, so a partition still being
/// appended to is read only up to its last durable record separator.
fn newline_aligned_size(path: &Path, size: u64) -> Result<u64> {
    if size == 0 {
        return Ok(0);
    }
    let handle = File::open(path)?;
    let mut buffer = vec![0u8; std::cmp::min(READ_CHUNK as u64, size) as usize];
    let mut end = size;
    while end > 0 {
        let start = end.saturating_sub(buffer.len() as u64);
        let length = (end - start) as usize;
        read_exact_at(&handle, &mut buffer[..length], start)?;
        if let Some(at) = buffer[..length].iter().rposition(|byte| *byte == b'\n') {
            return Ok(start + at as u64 + 1);
        }
        end = start;
    }
    Ok(0)
}

fn read_exact_at(handle: &File, buffer: &mut [u8], offset: u64) -> Result<()> {
    use std::os::unix::fs::FileExt;
    handle.read_exact_at(buffer, offset)?;
    Ok(())
}

/// One append-only partition file, measured the way the export cursor records it.
struct Partition {
    runtime: String,
    path: PathBuf,
    size: u64,
    physical_size: u64,
    mtime_ms: f64,
}

// Oko imports this materialized per-session view. Lake remains the sole parser
// of vendor formats; Oko decodes the stable canonical rows written here.
fn event_partition_files(data_dir: &Path) -> Result<Vec<Partition>> {
    let mut files = Vec::new();
    let events_root = data_dir.join("events");
    for runtime_name in read_dir_names(&events_root)? {
        if !runtime_name.starts_with("runtime=") || runtime_name == "runtime=hooks" {
            continue;
        }
        let runtime_dir = events_root.join(&runtime_name);
        for date_name in read_dir_names(&runtime_dir)? {
            if !date_name.starts_with("date=") {
                continue;
            }
            let date_dir = runtime_dir.join(&date_name);
            for part_name in read_dir_names(&date_dir)? {
                if !part_name.starts_with("part-") || !part_name.ends_with(".ndjson") {
                    continue;
                }
                let path = date_dir.join(&part_name);
                let meta = fs::metadata(&path)?;
                files.push(Partition {
                    runtime: runtime_name["runtime=".len()..].to_string(),
                    size: newline_aligned_size(&path, meta.len())?,
                    physical_size: meta.len(),
                    mtime_ms: mtime_ms(&meta),
                    path,
                });
            }
        }
    }
    files.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(files)
}

fn session_key(runtime: &str, session_id: &str) -> String {
    format!("{runtime}\n{session_id}")
}

fn hash_text(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

/// `String(value)` as JavaScript performs it for the values a canonical row
/// can carry.
fn js_string(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(true) => "true".to_string(),
        Value::Bool(false) => "false".to_string(),
        Value::Number(number) => number.to_string(),
        Value::String(text) => text.clone(),
        Value::Array(items) => items
            .iter()
            .map(|item| if item.is_null() { String::new() } else { js_string(item) })
            .collect::<Vec<_>>()
            .join(","),
        Value::Object(_) => "[object Object]".to_string(),
    }
}

fn is_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64().is_some_and(|raw| raw != 0.0 && !raw.is_nan()),
        Value::String(text) => !text.is_empty(),
        _ => true,
    }
}

/// `String(value || '')`: an absent or falsy field contributes nothing.
fn coerce_text(value: Option<&Value>) -> String {
    match value {
        Some(value) if is_truthy(value) => js_string(value),
        _ => String::new(),
    }
}

/// Whole numbers are published as JSON integers, matching how the previous
/// implementation serialized doubles that carry no fraction.
fn number_value(raw: f64) -> Value {
    if raw.is_finite() && raw.fract() == 0.0 && raw.abs() < 9007199254740992.0 {
        return Value::from(raw as i64);
    }
    Value::from(raw)
}

// Deterministic per-event id dedupes explicit full re-ingests and gives Oko
// stable tool-use identifiers without retaining a source filename.
fn fingerprint(event: &Value, runtime: &str) -> String {
    let extra = match event.get("extra") {
        Some(extra) if is_truthy(extra) => {
            serde_json::to_string(extra).unwrap_or_else(|_| "{}".to_string())
        }
        _ => "{}".to_string(),
    };
    let mut hash = Sha256::new();
    let fields = [
        runtime.to_string(),
        coerce_text(event.get("session_id")),
        coerce_text(event.get("ts")),
        coerce_text(event.get("event_type")),
        coerce_text(event.get("text")),
        coerce_text(event.get("tool_name")),
        coerce_text(event.get("model")),
        extra,
    ];
    for field in fields {
        hash.update(field.as_bytes());
        hash.update(b"\n");
    }
    format!("{:x}", hash.finalize())[..FP_LEN].to_string()
}

fn optional_string(event: &Value, key: &str) -> Value {
    match event.get(key) {
        Some(Value::String(text)) => Value::String(text.clone()),
        _ => Value::Null,
    }
}

fn optional_number(event: &Value, key: &str) -> Value {
    match event.get(key) {
        Some(Value::Number(number)) => Value::Number(number.clone()),
        _ => Value::Null,
    }
}

/// The exported row, in the field order Oko's importer reads.
fn export_line(event: &Value, runtime: &str, fingerprint: &str) -> Value {
    let mut row = Map::new();
    row.insert("lake_schema".to_string(), json!("oko-import-v1"));
    row.insert("uuid".to_string(), json!(fingerprint));
    row.insert("ts".to_string(), event.get("ts").cloned().unwrap_or(Value::Null));
    row.insert("runtime".to_string(), json!(runtime));
    row.insert(
        "session_id".to_string(),
        event.get("session_id").cloned().unwrap_or(Value::Null),
    );
    row.insert("project".to_string(), optional_string(event, "project"));
    row.insert(
        "event_type".to_string(),
        event.get("event_type").cloned().unwrap_or(Value::Null),
    );
    row.insert(
        "text".to_string(),
        match event.get("text") {
            Some(Value::String(text)) => Value::String(text.clone()),
            _ => json!(""),
        },
    );
    row.insert("tool_name".to_string(), optional_string(event, "tool_name"));
    row.insert("model".to_string(), optional_string(event, "model"));
    row.insert("tokens_in".to_string(), optional_number(event, "tokens_in"));
    row.insert("tokens_out".to_string(), optional_number(event, "tokens_out"));
    row.insert(
        "extra".to_string(),
        match event.get("extra") {
            Some(extra @ Value::Object(_)) => extra.clone(),
            _ => Value::Object(Map::new()),
        },
    );
    Value::Object(row)
}

fn atomic_write(file: &Path, content: &str) -> Result<()> {
    if let Some(parent) = file.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = PathBuf::from(format!("{}.tmp-{}", file.display(), std::process::id()));
    fs::write(&temporary, content)?;
    fs::rename(&temporary, file)?;
    Ok(())
}

fn remove_tree(path: &Path) -> Result<()> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn read_text(file: &Path) -> Option<String> {
    fs::read(file).ok().map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
}

/// Lines of one byte range of a partition, decoded the way Node's utf8 stream
/// decodes them: an invalid sequence becomes a replacement character rather
/// than failing the export.
struct LineReader {
    inner: std::io::Take<BufReader<File>>,
    buffer: Vec<u8>,
}

impl LineReader {
    fn open(path: &Path, start: u64, end: u64) -> Result<Self> {
        let mut file = File::open(path)?;
        if start > 0 {
            file.seek(SeekFrom::Start(start))?;
        }
        Ok(Self {
            inner: BufReader::new(file).take(end.saturating_sub(start)),
            buffer: Vec::with_capacity(4096),
        })
    }

    fn next_line(&mut self) -> Result<Option<String>> {
        self.buffer.clear();
        if self.inner.read_until(b'\n', &mut self.buffer)? == 0 {
            return Ok(None);
        }
        let mut line = self.buffer.as_slice();
        if line.last() == Some(&b'\n') {
            line = &line[..line.len() - 1];
        }
        if line.last() == Some(&b'\r') {
            line = &line[..line.len() - 1];
        }
        Ok(Some(String::from_utf8_lossy(line).into_owned()))
    }
}

#[derive(Default)]
struct Tally {
    malformed: u64,
    last_error: Option<String>,
}

/// A conversation row that carries an identity and a timestamp.
fn accepted(event: &Value) -> bool {
    if !event.is_object() {
        return false;
    }
    let Some(event_type) = event.get("event_type").and_then(Value::as_str) else {
        return false;
    };
    if !CONVERSATION_EVENTS.contains(&event_type) {
        return false;
    }
    if !event.get("session_id").and_then(Value::as_str).is_some_and(|id| !id.is_empty()) {
        return false;
    }
    event.get("ts").and_then(Value::as_str).is_some_and(|ts| !ts.is_empty())
}

fn row_runtime(event: &Value, fallback: &str) -> String {
    match event.get("runtime").and_then(Value::as_str) {
        Some(runtime) if !runtime.is_empty() => runtime.to_string(),
        _ => fallback.to_string(),
    }
}

struct StagedSession {
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
fn stage_events(
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
            let staged_file = staging_root.join(&runtime).join(session_hash.clone() + ".ndjson");
            let fingerprint = fingerprint(&event, &runtime);
            let chunk = serde_json::to_string(&export_line(&event, &runtime, &fingerprint))? + "\n";
            buffered_bytes += chunk.len();
            buffers.entry(staged_file.clone()).or_default().push_str(&chunk);
            if seen.insert(key) {
                sessions.push(StagedSession { runtime, session_hash, staged_file });
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

struct SessionWrite {
    file: PathBuf,
    records: u64,
    changed: bool,
}

fn session_file(output_root: &Path, runtime: &str, session_hash: &str) -> PathBuf {
    output_root.join(format!("runtime={runtime}")).join(format!("{session_hash}.jsonl"))
}

fn row_order(left: &Value, right: &Value) -> std::cmp::Ordering {
    let text = |row: &Value, key: &str| match row.get(key) {
        Some(value) => js_string(value),
        None => "undefined".to_string(),
    };
    text(left, "ts").cmp(&text(right, "ts")).then_with(|| text(left, "uuid").cmp(&text(right, "uuid")))
}

fn dedupe(rows: Vec<Value>) -> Vec<Value> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut unique = Vec::with_capacity(rows.len());
    for row in rows {
        let uuid = match row.get("uuid") {
            Some(uuid) => js_string(uuid),
            None => "undefined".to_string(),
        };
        if seen.insert(uuid) {
            unique.push(row);
        }
    }
    unique
}

fn render(rows: &[Value]) -> Result<String> {
    let mut content = String::new();
    for (index, row) in rows.iter().enumerate() {
        if index > 0 {
            content.push('\n');
        }
        content.push_str(&serde_json::to_string(row)?);
    }
    content.push('\n');
    Ok(content)
}

fn materialize_session(entry: &StagedSession, output_root: &Path) -> Result<SessionWrite> {
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
        return Ok(SessionWrite { file, records: rows.len() as u64, changed: false });
    }
    atomic_write(&file, &content)?;
    Ok(SessionWrite { file, records: rows.len() as u64, changed: true })
}

fn prune_outputs(output_root: &Path, expected: &HashSet<PathBuf>) -> Result<u64> {
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

fn read_export_cursors(file: &Path) -> Option<Map<String, Value>> {
    if !file.exists() {
        return None;
    }
    match serde_json::from_str::<Value>(&read_text(file)?) {
        Ok(Value::Object(store)) => Some(store),
        _ => None,
    }
}

fn partition_snapshot(partitions: &[Partition]) -> Value {
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

struct IncrementalSession {
    runtime: String,
    session_hash: String,
    rows: Vec<Value>,
}

/// Rows appended since the recorded cursor, or `None` when a partition was
/// truncated or rewritten in place and only a staging rebuild is sound.
fn incremental_sessions(
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
            let mtime_changed =
                cursor.mtime_ms.is_none_or(|recorded| partition.mtime_ms != recorded);
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
            sessions[slot].rows.push(export_line(&event, &runtime, &fingerprint));
        }
    }
    Ok(Some(sessions))
}

fn merge_incremental_session(
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
        return Ok(SessionWrite { file, records, changed: false });
    }
    atomic_write(&file, &content)?;
    Ok(SessionWrite { file, records, changed: true })
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
    let cursors = if full { None } else { read_export_cursors(&cursor_file) };
    let result = match &cursors {
        None => full_export(&partitions, &output_root, &staging_root, &mut tally)?,
        Some(cursors) => {
            match incremental_sessions(&partitions, cursors, &mut tally)? {
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
                        pruned: 0,
                        mode: "incremental",
                    }
                }
            }
        }
    };
    atomic_write(&cursor_file, &(pretty_one_space(&partition_snapshot(&partitions))? + "\n"))?;
    let mut summary = Map::new();
    summary.insert("outputRoot".to_string(), json!(output_root.to_string_lossy()));
    summary.insert("sessions".to_string(), json!(result.sessions));
    summary.insert("records".to_string(), json!(result.records));
    summary.insert("written".to_string(), json!(result.written));
    summary.insert("unchanged".to_string(), json!(result.unchanged));
    summary.insert("pruned".to_string(), json!(result.pruned));
    summary.insert("mode".to_string(), json!(result.mode));
    summary.insert("malformed".to_string(), json!(tally.malformed));
    summary.insert("durationMs".to_string(), json!(started_at.elapsed().as_millis() as u64));
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

/// Materialize the Oko import view under the exclusive state lease.
pub fn export_oko(full: bool, data_dir: &Path) -> Result<Value> {
    export_oko_with_reindex(full, false, data_dir)
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
