//! Startup catch-up and filesystem-notification ingestion use one cursor contract.
pub(super) mod file;
mod owner;

use file::{log_changed_since, stream_file};

use super::{ingest_source_locked, total_hits, warn, Tally, Writer};
use crate::cursors::{open_writer_lease, CursorRecord, Cursors};
use crate::hook_segments::{catch_up_closed_hook_segments, stream_hook_segment, SegmentReport};
use crate::types::{Adapter, SessionEntry, HOOKS};
use crate::util::{home_dir, machine_name, Error, Result};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// What a pass did with each file of one runtime, kept as the files
/// themselves: its counts are their lengths, and a failed file can be named.
#[derive(Default)]
struct Outcomes {
    streamed: Vec<PathBuf>,
    unchanged: Vec<PathBuf>,
    failed: Vec<PathBuf>,
}

impl Outcomes {
    /// Add the files to the runtime's counters.
    fn count_into(&self, tally: &mut Tally) {
        tally.files += self.streamed.len() as u64;
        tally.skipped += self.unchanged.len() as u64;
        tally.failures += self.failed.len() as u64;
    }

    /// Stream one session file unless its cursor is current. A file that
    /// vanished since it was named has nothing to stream; one that cannot be
    /// inspected or streamed is a failure with its cause, not a file left
    /// unread in silence.
    fn stream(
        &mut self,
        writer: &mut Writer,
        cursors: &mut Cursors,
        adapter: &dyn Adapter,
        entry: &SessionEntry,
        tally: &mut Tally,
    ) -> Result<()> {
        let meta = match fs::metadata(&entry.file) {
            Ok(meta) => meta,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                warn(&format!(
                    "stat failed for {}: {error}",
                    entry.file.display()
                ));
                self.failed.push(entry.file.clone());
                return Ok(());
            }
        };
        let key = entry.file.to_string_lossy().to_string();
        if let Some(CursorRecord::Bytes(cursor)) = cursors.get(&key)? {
            if cursor.is_current(&meta) && !log_changed_since(&entry.file, cursor.mtime_ms) {
                self.unchanged.push(entry.file.clone());
                return Ok(());
            }
        }
        let before = total_hits(&writer.masker.counts());
        match stream_file(writer, cursors, adapter, entry, false, tally) {
            Ok(()) => self.streamed.push(entry.file.clone()),
            Err(error) => {
                warn(&format!("{}: {error}", entry.file.display()));
                self.failed.push(entry.file.clone());
            }
        }
        tally.masked_hits += total_hits(&writer.masker.counts()) - before;
        Ok(())
    }
}

/// A hook segment pass's report added to the hooks counters.
fn count_segments(report: &SegmentReport, tally: &mut Tally) {
    tally.files += report.files;
    tally.events += report.events;
    tally.skipped += report.skipped;
    tally.failures += report.invalid;
}

/// The summary both passes end with: the per-runtime counters, the mask
/// counts, and how many files failed, which makes the pass partial. Each
/// failed file was named on stderr with its cause when it failed.
fn summary(
    per_runtime: Map<String, Value>,
    writer: &Writer,
    started: Instant,
    streamed: &[u64],
    failed: &[PathBuf],
) -> Value {
    json!({
        "perRuntime": Value::Object(per_runtime),
        "maskCounts": writer.masker.counts(),
        "durationMs": started.elapsed().as_millis() as u64,
        "filesStreamed": streamed.iter().sum::<u64>(),
        "partial": !failed.is_empty(),
        "failures": failed.len() as u64,
    })
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
    if let Some(selected) = crate::sources::selected_source(data_dir)? {
        return ingest_source_locked(data_dir, &selected.runtime, &selected.root);
    }
    let started = Instant::now();
    let home = home_dir();
    let hook_sources = crate::paths::hook_source_roots();
    let mut adapters = crate::adapters::all();
    if !hook_sources.segment_mode && hook_sources.available {
        adapters.push(crate::hook_segments::hooks_adapter());
    }
    let mut cursors = Cursors::open(data_dir)?;
    let mut writer = Writer::new(data_dir.to_path_buf(), machine_name())?;
    let mut per_runtime = Map::new();
    let mut discovered = Vec::new();
    let mut streamed = Vec::new();
    let mut failed = Vec::new();

    for adapter in adapters {
        let mut tally = Tally::default();
        let mut outcomes = Outcomes::default();
        for root in adapter.roots(&home) {
            let entries = adapter.list_sessions(&root);
            discovered.push(entries.len() as u64);
            for entry in &entries {
                outcomes.stream(
                    &mut writer,
                    &mut cursors,
                    adapter.as_ref(),
                    entry,
                    &mut tally,
                )?;
            }
        }
        outcomes.count_into(&mut tally);
        streamed.push(outcomes.streamed.len() as u64);
        failed.extend(outcomes.failed);
        per_runtime.insert(adapter.runtime().to_string(), serde_json::to_value(tally)?);
    }
    if hook_sources.segment_mode {
        let before = total_hits(&writer.masker.counts());
        let report = catch_up_closed_hook_segments(
            &hook_sources.ready,
            data_dir,
            &mut cursors,
            &mut writer,
        )?;
        let mut tally = Tally::default();
        count_segments(&report, &mut tally);
        tally.masked_hits = total_hits(&writer.masker.counts()) - before;
        discovered.push(report.files + report.skipped + report.invalid);
        streamed.push(report.files);
        failed.extend(report.invalid_segments);
        per_runtime.insert(HOOKS.to_string(), serde_json::to_value(tally)?);
    }

    cursors.flush()?;
    let mut report = summary(per_runtime, &writer, started, &streamed, &failed);
    report["filesDiscovered"] = json!(discovered.iter().sum::<u64>());
    Ok(report)
}

/// Stream exactly the source files named by filesystem notifications.
///
/// Changed files verify their consumed prefix before reading new lines.
/// Paths outside the adapter contracts are ignored without a root walk,
/// and Oko is projected in the same transaction as canonical rows.
pub fn stream_paths(data_dir: &Path, paths: &[PathBuf]) -> Result<Value> {
    let mut lease = open_writer_lease(data_dir)?;
    let summary = stream_paths_locked(data_dir, paths);
    lease.close();
    summary
}

fn stream_paths_locked(data_dir: &Path, paths: &[PathBuf]) -> Result<Value> {
    let started = Instant::now();
    let home = home_dir();
    let selected = crate::sources::selected_source(data_dir)?;
    let hook_sources = crate::paths::hook_source_roots();
    let mut adapters = if let Some(source) = &selected {
        vec![crate::adapters::by_name(&source.runtime).ok_or_else(|| {
            Error(format!(
                "transcript adapter unavailable: {}",
                source.runtime
            ))
        })?]
    } else {
        crate::adapters::all()
    };
    if selected.is_none() && !hook_sources.segment_mode && hook_sources.available {
        adapters.push(crate::hook_segments::hooks_adapter());
    }
    let mut cursors = Cursors::open(data_dir)?;
    let mut writer = Writer::new(data_dir.to_path_buf(), machine_name())?;
    let mut runs: HashMap<&'static str, (Tally, Outcomes)> = HashMap::new();
    let mut streamed = Vec::new();
    let mut failed = Vec::new();

    for path in paths {
        if selected.is_none() && hook_sources.segment_mode && path.starts_with(&hook_sources.ready)
        {
            let (tally, _) = runs.entry(HOOKS).or_default();
            let before = total_hits(&writer.masker.counts());
            let report = stream_hook_segment(path, data_dir, &mut cursors, &mut writer)?;
            count_segments(&report, tally);
            tally.masked_hits += total_hits(&writer.masker.counts()) - before;
            streamed.push(report.files);
            failed.extend(report.invalid_segments);
            continue;
        }
        match owner::owner(&adapters, selected.as_ref(), &home, path) {
            owner::Resolved::Session(adapter, entry) => {
                let (tally, outcomes) = runs.entry(adapter.runtime()).or_default();
                outcomes.stream(&mut writer, &mut cursors, adapter, &entry, tally)?;
            }
            owner::Resolved::Unowned => {}
            owner::Resolved::Unreadable(runtime, error) => {
                warn(&format!(
                    "cannot tell whether {} is a {runtime} session: {error}",
                    path.display()
                ));
                runs.entry(runtime).or_default().1.failed.push(path.clone());
            }
        }
    }

    cursors.flush()?;
    let mut per_runtime: Map<String, Value> = Map::new();
    for (runtime, (mut tally, outcomes)) in runs {
        outcomes.count_into(&mut tally);
        streamed.push(outcomes.streamed.len() as u64);
        failed.extend(outcomes.failed);
        per_runtime.insert(runtime.to_string(), serde_json::to_value(&tally)?);
    }
    Ok(summary(per_runtime, &writer, started, &streamed, &failed))
}
