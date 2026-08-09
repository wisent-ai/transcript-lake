//! Real-time transcript stream. Source notifications name the file that moved;
//! the reader resumes that append-only file at its durable byte cursor, masks
//! each canonical event, writes its Lake partition, and updates Oko's session
//! projection before advancing the cursor.
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::Serialize;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::cursors::{open_writer_lease, ByteCursor, CursorRecord, Cursors};
use crate::hook_segments::{replay_closed_hook_segments, stream_hook_segment};
use crate::redact::Masker;
use crate::types::{
    Adapter, CanonicalEvent, EventSink, ParserCtx, RawEvent, SegmentOutput, SessionEntry, HOOKS,
    SUPPORTED_SOURCES,
};
use crate::util::{home_dir, machine_name, mtime_ms, Error, Result};

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

/// Cut to the text cap counting UTF-16 units, which is what the previous
/// implementation's `String.length` counted. A byte length within the cap can
/// never exceed it in UTF-16 units, so the common case never scans the string.
///
/// One deliberate deviation, the only one measured across the ported corpus:
/// when the cap lands mid astral character, `slice` left a lone high surrogate
/// behind and serialized it as `\ud83d`, which is not valid Unicode and which
/// a Rust `String` cannot hold. The character is kept out instead, so such a
/// line is one character shorter than the previous implementation wrote.
fn clip(mut text: String) -> String {
    if text.len() <= TEXT_CAP {
        return text;
    }
    let mut units = 0usize;
    let mut end = text.len();
    for (index, character) in text.char_indices() {
        let width = character.len_utf16();
        if units + width > TEXT_CAP {
            end = index;
            break;
        }
        units += width;
    }
    text.truncate(end);
    text
}

/// The date partition an event belongs to: the leading `YYYY-MM-DD` of its
/// timestamp. An unusable timestamp lands in a visible catch-all partition,
/// not dropped.
fn date_of(ts: Option<&str>) -> String {
    let Some(ts) = ts else {
        return "unknown".to_string();
    };
    let bytes = ts.as_bytes();
    let shaped = bytes.len() >= 10
        && bytes[..4].iter().all(u8::is_ascii_digit)
        && bytes[4] == b'-'
        && bytes[5..7].iter().all(u8::is_ascii_digit)
        && bytes[7] == b'-'
        && bytes[8..10].iter().all(u8::is_ascii_digit);
    if shaped {
        ts[..10].to_string()
    } else {
        "unknown".to_string()
    }
}

/// Masks every string inside extra, to a small depth bound (extra stays
/// small). Non-string leaves pass through untouched; JSON serialization later
/// renders them exactly as the adapter emitted them.
fn mask_deep(value: Value, masker: &mut Masker, depth: i32) -> Value {
    match value {
        Value::String(text) => Value::String(clip(masker.mask(&text))),
        Value::Array(items) => {
            if depth <= 0 {
                return Value::Null;
            }
            Value::Array(
                items
                    .into_iter()
                    .map(|item| mask_deep(item, masker, depth - 1))
                    .collect(),
            )
        }
        Value::Object(fields) => {
            if depth <= 0 {
                return Value::Null;
            }
            Value::Object(
                fields
                    .into_iter()
                    .map(|(key, item)| (key, mask_deep(item, masker, depth - 1)))
                    .collect(),
            )
        }
        other => other,
    }
}

/// The single masking boundary: every canonical event in the Lake is produced
/// here, so no adapter can route unmasked text to a partition.
pub struct Writer {
    data_dir: PathBuf,
    machine: String,
    masker: Masker,
}

impl Writer {
    fn new(data_dir: PathBuf, machine: String) -> Self {
        Self {
            data_dir,
            machine,
            masker: Masker::new(),
        }
    }

    fn canonicalize(&mut self, event: &RawEvent, runtime: &str) -> (CanonicalEvent, String) {
        let date = date_of(event.ts.as_deref());
        let text = clip(self.masker.mask(&event.text));
        let extra = mask_deep(
            Value::Object(event.extra.clone()),
            &mut self.masker,
            EXTRA_DEPTH,
        );
        let canonical = CanonicalEvent {
            ts: event.ts.clone(),
            runtime: runtime.to_string(),
            machine: self.machine.clone(),
            session_id: event.session_id.clone(),
            project: event.project.clone(),
            event_type: if event.event_type.is_empty() {
                "meta".to_string()
            } else {
                event.event_type.clone()
            },
            text,
            tool_name: event.tool_name.clone(),
            model: event.model.clone(),
            tokens_in: event.tokens_in,
            tokens_out: event.tokens_out,
            // A depth bound can null the whole map only when extra is nested
            // deeper than the bound, which the object itself never is.
            extra: match extra {
                Value::Object(fields) => fields,
                _ => Map::new(),
            },
        };
        (canonical, date)
    }

    /// Persist one source delta to canonical partitions and Oko's live
    /// per-session projection before the source cursor can advance.
    fn write_batch(
        &mut self,
        events: &[RawEvent],
        runtime: &str,
        part_name: &str,
        stem_hash: &str,
        tally: &mut Tally,
    ) -> Result<()> {
        let mut rows: Vec<(PathBuf, String)> = Vec::with_capacity(events.len());
        let mut projections: Vec<Value> = Vec::with_capacity(events.len());
        for event in events {
            let (mut canonical, date) = self.canonicalize(event, runtime);
            canonical.extra.insert(
                "source_stem_hash".to_string(),
                Value::String(stem_hash.to_string()),
            );
            let dir = self
                .data_dir
                .join("events")
                .join(format!("runtime={runtime}"))
                .join(format!("date={date}"));
            rows.push((dir, serde_json::to_string(&canonical)?));
            projections.push(serde_json::to_value(canonical)?);
            tally.events += 1;
        }
        let mut dirs: Vec<&PathBuf> = Vec::new();
        for (dir, _) in &rows {
            if !dirs.contains(&dir) {
                dirs.push(dir);
            }
        }
        for dir in dirs {
            fs::create_dir_all(dir)?;
            let mut payload = String::new();
            for (row_dir, line) in &rows {
                if row_dir == dir {
                    payload.push_str(line);
                    payload.push('\n');
                }
            }
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(dir.join(part_name))?;
            file.write_all(payload.as_bytes())?;
        }
        crate::oko_export::project_events(&self.data_dir, projections)?;
        Ok(())
    }
}

fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)?.sync_all()?;
    Ok(())
}

/// Whole-file publish for immutable output: unique temp, sync, rename, then
/// parent sync, so a reader never observes a partial segment partition.
fn durable_write(path: &Path, content: &str) -> Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let name = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let temporary = parent.join(format!(".{name}.{}.tmp", uuid::Uuid::new_v4()));
    let write = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(content.as_bytes())?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)?;
        sync_directory(parent)
    })();
    if write.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    write
}

/// Closed segments are immutable and content-addressed: republishing the same
/// bytes is a no-op, republishing different bytes under the same name is a
/// refusal, never an overwrite.
impl EventSink for Writer {
    fn accept(&mut self, source_file: &Path, events: &[RawEvent]) -> Result<Vec<SegmentOutput>> {
        let stem = source_file
            .file_name()
            .map(|name| file_stem(&name.to_string_lossy()))
            .unwrap_or_default();
        let part_name = format!("{stem}.ndjson");
        // First-appearance order, exactly like the Map the previous
        // implementation grouped into.
        let mut groups: Vec<(String, Vec<String>)> = Vec::new();
        for event in events {
            let (canonical, date) = self.canonicalize(event, HOOKS);
            let line = serde_json::to_string(&canonical)?;
            match groups.iter_mut().find(|(known, _)| *known == date) {
                Some((_, lines)) => lines.push(line),
                None => groups.push((date, vec![line])),
            }
        }
        let mut outputs = Vec::with_capacity(groups.len());
        for (date, lines) in groups {
            let mut content = lines.join("\n");
            content.push('\n');
            let sha256 = hex_digest(content.as_bytes());
            let path = self
                .data_dir
                .join("events")
                .join(format!("runtime={HOOKS}"))
                .join(format!("date={date}"))
                .join(&part_name);
            if path.exists() {
                if hex_digest(&fs::read(&path)?) != sha256 {
                    return Err(Error(format!(
                        "hook segment output conflict: {}",
                        path.display()
                    )));
                }
            } else {
                durable_write(&path, &content)?;
            }
            outputs.push(SegmentOutput { path, sha256 });
        }
        Ok(outputs)
    }
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

/// Streams one source file from its resume offset, byte-accurate so cursor
/// checkpoints always land on line boundaries even with multibyte text.
#[allow(clippy::too_many_arguments)]
fn stream_file(
    writer: &mut Writer,
    cursors: &mut Cursors,
    adapter: &dyn Adapter,
    entry: &SessionEntry,
    meta: &fs::Metadata,
    replay: bool,
    tally: &mut Tally,
) -> Result<()> {
    let key = entry.file.to_string_lossy().to_string();
    let size = meta.len();
    let modified = mtime_ms(meta);
    let current = match if replay { None } else { cursors.get(&key)? } {
        Some(CursorRecord::Bytes(cursor)) => Some(cursor),
        // A tagged segment commit is keyed by segment id, never by a source
        // path, so it can only mean this path is not a byte stream we resume.
        Some(CursorRecord::Segment(_)) | None => None,
    };
    if let Some(cursor) = current {
        if size < cursor.size {
            return Err(Error(
                "source shrank after its last checkpoint; preserve the Lake and use rebuild".into(),
            ));
        }
        if size == cursor.size && cursor.mtime_ms != modified && cursor.offset >= size {
            return Err(Error(
                "source changed without an append; preserve the Lake and use rebuild".into(),
            ));
        }
    }
    let offset = current.map(|cursor| cursor.offset.min(size)).unwrap_or(0);
    let mut parser = adapter.parser(ParserCtx {
        file: entry.file.clone(),
        session_id: entry.session_id.clone(),
        project: entry.project.clone(),
    });
    let digest = hex_digest(key.as_bytes());
    let part_name = format!("part-{}.ndjson", &digest[..PART_DIGEST_LEN]);
    let stem_hash = hex_digest(
        file_stem(&entry.file.file_name().unwrap_or_default().to_string_lossy()).as_bytes(),
    );
    let runtime = adapter.runtime();

    let mut file = File::open(&entry.file)?;
    file.seek(SeekFrom::Start(offset))?;
    let mut reader = BufReader::with_capacity(READ_BUFFER, file);
    let mut batch: Vec<RawEvent> = Vec::new();
    let mut consumed = offset;
    let mut raw: Vec<u8> = Vec::new();
    loop {
        raw.clear();
        let read = reader.read_until(b'\n', &mut raw)?;
        if read == 0 {
            break;
        }
        // A trailing line without its newline is a runtime still writing: it
        // is neither parsed nor counted as consumed, so the next run resumes
        // at its first byte.
        if raw[read - 1] != b'\n' {
            break;
        }
        consumed += read as u64;
        let mut line = &raw[..read - 1];
        if line.last() == Some(&b'\r') {
            line = &line[..line.len() - 1];
        }
        batch.extend(parser.on_line(&String::from_utf8_lossy(line)));
        if batch.len() >= BATCH_EVENTS {
            checkpoint(
                writer, cursors, &mut batch, runtime, &part_name, &stem_hash, tally, &key,
                modified, size, consumed,
            )?;
        }
    }
    batch.extend(parser.end());
    checkpoint(
        writer, cursors, &mut batch, runtime, &part_name, &stem_hash, tally, &key, modified, size,
        consumed,
    )
}

/// Flush the buffered events, then record the byte offset they cover. The
/// order matters: a crash between the two replays events, never skips them.
#[allow(clippy::too_many_arguments)]
fn checkpoint(
    writer: &mut Writer,
    cursors: &mut Cursors,
    batch: &mut Vec<RawEvent>,
    runtime: &str,
    part_name: &str,
    stem_hash: &str,
    tally: &mut Tally,
    key: &str,
    mtime: f64,
    size: u64,
    consumed: u64,
) -> Result<()> {
    if !batch.is_empty() {
        writer.write_batch(batch, runtime, part_name, stem_hash, tally)?;
        batch.clear();
    }
    cursors.set_bytes(
        key,
        ByteCursor {
            mtime_ms: mtime,
            size,
            offset: consumed,
        },
    );
    cursors.flush()
}

fn total_hits(counts: &crate::redact::MaskCounts) -> u64 {
    counts.token + counts.entropy + counts.assignment
}

/// Where closed hook segments are published for pickup.
fn segments_ready_dir() -> PathBuf {
    std::env::var_os("HOOKS_ADAPTIVE_SEGMENTS_READY")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| {
            home_dir()
                .join(".hooks-adaptive")
                .join("telemetry-segments")
                .join("ready")
        })
}

fn replay_locked(opts: &ReplayOptions) -> Result<Value> {
    let started = Instant::now();
    let data_dir = opts.data_dir.clone();
    let machine = machine_name();
    let requested = opts.source.as_deref();
    if let Some(name) = requested {
        if !SUPPORTED_SOURCES.contains(&name) {
            return Err(Error(format!(
                "unknown source \"{name}\" (expected one of: {})",
                SUPPORTED_SOURCES.join(", ")
            )));
        }
    }
    let selected: Vec<&str> = match requested {
        Some(name) => vec![name],
        None => SUPPORTED_SOURCES.to_vec(),
    };
    let mut cursors = Cursors::open(&data_dir)?;
    let mut writer = Writer::new(data_dir.clone(), machine);
    let home = home_dir();
    let mut per_runtime = Map::new();
    for name in selected {
        let mut tally = Tally::default();
        let before = total_hits(&writer.masker.counts());
        let mut adapter: Option<Box<dyn Adapter>> = None;
        if name == HOOKS {
            let ready = segments_ready_dir();
            if ready.exists() {
                // An integrity failure inside a segment commit (an output or
                // acknowledgement that disagrees with what is already on
                // disk) aborts the run rather than counting a failure and
                // continuing to write, exactly as it does today.
                let report =
                    replay_closed_hook_segments(&ready, &data_dir, &mut cursors, &mut writer)?;
                tally.files += report.files;
                tally.events += report.events;
                tally.skipped += report.skipped;
                tally.failures += report.invalid;
            } else {
                // Still one mutable telemetry log rather than closed
                // segments: the same producer, read as a line adapter.
                adapter = Some(crate::hook_segments::hooks_adapter());
            }
        } else {
            adapter = crate::adapters::by_name(name);
            if adapter.is_none() {
                warn(&format!("adapter \"{name}\" unavailable, runtime skipped"));
                tally.failures += 1;
            }
        }
        let Some(adapter) = adapter else {
            // No line adapter for this runtime: either the closed-segment
            // reader already did the work, or the runtime was skipped. The
            // previous implementation left the loop here without recording
            // maskedHits, so a segment-mode hooks run always reported zero
            // however much it masked. The hits are counted here instead: the
            // number is an observability counter, not part of any on-disk
            // format. Its trailing cursor flush is still skipped, because a
            // segment commit is already flushed as part of the commit.
            tally.masked_hits = total_hits(&writer.masker.counts()) - before;
            per_runtime.insert(name.to_string(), serde_json::to_value(&tally)?);
            continue;
        };
        {
            for root in adapter.roots(&home) {
                for entry in adapter.list_sessions(&root) {
                    let meta = match fs::metadata(&entry.file) {
                        Ok(meta) => meta,
                        Err(error) => {
                            warn(&format!(
                                "stat failed for {}: {error}",
                                entry.file.display()
                            ));
                            tally.failures += 1;
                            continue;
                        }
                    };
                    match stream_file(
                        &mut writer,
                        &mut cursors,
                        adapter.as_ref(),
                        &entry,
                        &meta,
                        true,
                        &mut tally,
                    ) {
                        Ok(()) => tally.files += 1,
                        Err(error) => {
                            warn(&format!("{}: {error}", entry.file.display()));
                            tally.failures += 1;
                        }
                    }
                }
            }
        }
        tally.masked_hits = total_hits(&writer.masker.counts()) - before;
        cursors.flush()?;
        per_runtime.insert(name.to_string(), serde_json::to_value(&tally)?);
    }
    let failures: u64 = per_runtime
        .values()
        .filter_map(|tally| tally.get("failures").and_then(Value::as_u64))
        .sum();
    Ok(json!({
        "perRuntime": Value::Object(per_runtime),
        "maskCounts": writer.masker.counts(),
        "durationMs": started.elapsed().as_millis() as u64,
        "partial": failures > 0,
        "failures": failures,
    }))
}

/// Replay every selected source into a separate empty Lake for recovery.
pub fn replay(opts: ReplayOptions) -> Result<Value> {
    let occupied = fs::read_dir(&opts.data_dir)
        .map(|mut entries| entries.next().is_some())
        .unwrap_or(false);
    if occupied {
        return Err(Error(
            "rebuild requires an empty LAKE_DATA root so replay cannot duplicate or erase existing evidence"
                .into(),
        ));
    }
    let mut lease = open_writer_lease(&opts.data_dir)?;
    let summary = replay_locked(&opts);
    lease.close();
    summary
}

/// Close source cursor gaps left while the service was stopped, then hand off
/// to filesystem notifications. Adapters enumerate once at startup and their
/// already-derived entries go straight to the writer without re-discovering a
/// runtime root for every file.
pub fn catch_up(data_dir: &Path) -> Result<Value> {
    let mut lease = open_writer_lease(data_dir)?;
    let summary = catch_up_locked(data_dir);
    lease.close();
    summary
}

fn catch_up_locked(data_dir: &Path) -> Result<Value> {
    let started = Instant::now();
    let home = home_dir();
    let hook_sources = crate::paths::hook_source_roots();
    let mut adapters = crate::adapters::all();
    if !hook_sources.segment_mode && hook_sources.available {
        adapters.push(crate::hook_segments::hooks_adapter());
    }
    let mut cursors = Cursors::open(&data_dir.to_path_buf())?;
    let mut writer = Writer::new(data_dir.to_path_buf(), machine_name());
    let mut per_runtime = Map::new();
    let mut discovered = 0u64;
    let mut touched = 0u64;

    if hook_sources.segment_mode {
        let before = total_hits(&writer.masker.counts());
        let report =
            replay_closed_hook_segments(&hook_sources.ready, data_dir, &mut cursors, &mut writer)?;
        discovered += report.files + report.skipped + report.invalid;
        let tally = Tally {
            files: report.files,
            events: report.events,
            skipped: report.skipped,
            failures: report.invalid,
            masked_hits: total_hits(&writer.masker.counts()) - before,
        };
        touched += report.files;
        per_runtime.insert(HOOKS.to_string(), serde_json::to_value(tally)?);
    }

    for adapter in adapters {
        let mut tally = Tally::default();
        let before = total_hits(&writer.masker.counts());
        for root in adapter.roots(&home) {
            for entry in adapter.list_sessions(&root) {
                discovered += 1;
                let meta = match fs::metadata(&entry.file) {
                    Ok(meta) => meta,
                    Err(error) => {
                        warn(&format!(
                            "stat failed for {}: {error}",
                            entry.file.display()
                        ));
                        tally.failures += 1;
                        continue;
                    }
                };
                let key = entry.file.to_string_lossy().to_string();
                if let Some(CursorRecord::Bytes(cursor)) = cursors.get(&key)? {
                    if cursor.mtime_ms == mtime_ms(&meta)
                        && cursor.size == meta.len()
                        && cursor.offset >= meta.len()
                    {
                        tally.skipped += 1;
                        continue;
                    }
                }
                match stream_file(
                    &mut writer,
                    &mut cursors,
                    adapter.as_ref(),
                    &entry,
                    &meta,
                    false,
                    &mut tally,
                ) {
                    Ok(()) => {
                        tally.files += 1;
                        touched += 1;
                    }
                    Err(error) => {
                        warn(&format!("{}: {error}", entry.file.display()));
                        tally.failures += 1;
                    }
                }
            }
        }
        tally.masked_hits = total_hits(&writer.masker.counts()) - before;
        per_runtime.insert(adapter.runtime().to_string(), serde_json::to_value(tally)?);
    }

    cursors.flush()?;
    let failures = per_runtime
        .values()
        .filter_map(|tally| tally.get("failures").and_then(Value::as_u64))
        .sum::<u64>();
    Ok(json!({
        "perRuntime": Value::Object(per_runtime),
        "maskCounts": writer.masker.counts(),
        "durationMs": started.elapsed().as_millis() as u64,
        "filesDiscovered": discovered,
        "filesStreamed": touched,
        "partial": failures > 0,
        "failures": failures,
    }))
}

/// Stream exactly the source files named by filesystem notifications.
///
/// The cost is proportional to bytes appended since each file's durable
/// cursor. Paths outside the adapter contracts are ignored without a root
/// walk, and Oko is projected in the same transaction as canonical rows.
pub fn stream_paths(data_dir: &Path, paths: &[PathBuf]) -> Result<Value> {
    let mut lease = open_writer_lease(data_dir)?;
    let summary = stream_paths_locked(data_dir, paths);
    lease.close();
    summary
}

fn stream_paths_locked(data_dir: &Path, paths: &[PathBuf]) -> Result<Value> {
    let started = Instant::now();
    let home = home_dir();
    let hook_sources = crate::paths::hook_source_roots();
    let mut adapters = crate::adapters::all();
    if !hook_sources.segment_mode && hook_sources.available {
        adapters.push(crate::hook_segments::hooks_adapter());
    }
    let mut cursors = Cursors::open(&data_dir.to_path_buf())?;
    let mut writer = Writer::new(data_dir.to_path_buf(), machine_name());
    let mut per_runtime: Map<String, Value> = Map::new();
    let mut tallies: HashMap<&'static str, Tally> = HashMap::new();
    let mut touched = 0u64;

    for path in paths {
        if hook_sources.segment_mode && path.starts_with(&hook_sources.ready) {
            let tally = tallies.entry(HOOKS).or_default();
            let before = total_hits(&writer.masker.counts());
            let report = stream_hook_segment(path, data_dir, &mut cursors, &mut writer)?;
            tally.files += report.files;
            tally.events += report.events;
            tally.skipped += report.skipped;
            tally.failures += report.invalid;
            tally.masked_hits += total_hits(&writer.masker.counts()) - before;
            touched += report.files;
            continue;
        }
        let Some(adapter) = adapters.iter().find(|adapter| {
            adapter
                .roots(&home)
                .iter()
                .any(|root| path.starts_with(root))
        }) else {
            continue;
        };
        let Some(entry) = adapter.entry_for(path) else {
            continue;
        };
        let Ok(meta) = fs::metadata(&entry.file) else {
            continue;
        };
        let tally = tallies.entry(adapter.runtime()).or_default();
        let key = entry.file.to_string_lossy().to_string();
        if let Some(CursorRecord::Bytes(cursor)) = cursors.get(&key)? {
            if cursor.mtime_ms == mtime_ms(&meta)
                && cursor.size == meta.len()
                && cursor.offset >= meta.len()
            {
                tally.skipped += 1;
                continue;
            }
        }
        let before = total_hits(&writer.masker.counts());
        match stream_file(
            &mut writer,
            &mut cursors,
            adapter.as_ref(),
            &entry,
            &meta,
            false,
            tally,
        ) {
            Ok(()) => {
                tally.files += 1;
                touched += 1;
            }
            Err(error) => {
                warn(&format!("{}: {error}", entry.file.display()));
                tally.failures += 1;
            }
        }
        tally.masked_hits += total_hits(&writer.masker.counts()) - before;
    }

    cursors.flush()?;
    let mut failures = 0u64;
    for (runtime, tally) in tallies {
        failures += tally.failures;
        per_runtime.insert(runtime.to_string(), serde_json::to_value(&tally)?);
    }
    Ok(json!({
        "perRuntime": Value::Object(per_runtime),
        "maskCounts": writer.masker.counts(),
        "durationMs": started.elapsed().as_millis() as u64,
        "filesStreamed": touched,
        "partial": failures > 0,
        "failures": failures,
    }))
}
