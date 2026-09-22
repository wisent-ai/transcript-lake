//! Startup catch-up and filesystem-notification ingestion use one cursor contract.
pub(super) mod file;

use file::stream_file;

use super::{ingest_source_locked, total_hits, warn, Tally, Writer};
use crate::cursors::{open_writer_lease, CursorRecord, Cursors};
use crate::hook_segments::{catch_up_closed_hook_segments, stream_hook_segment};
use crate::types::HOOKS;
use crate::util::{home_dir, machine_name, Error, Result};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;
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
    let mut writer = Writer::new(data_dir.to_path_buf(), machine_name());
    let mut per_runtime = Map::new();
    let mut discovered = 0u64;
    let mut touched = 0u64;

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
                    if cursor.is_current(&meta) {
                        tally.skipped += 1;
                        continue;
                    }
                }
                match stream_file(
                    &mut writer,
                    &mut cursors,
                    adapter.as_ref(),
                    &entry,
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
    if hook_sources.segment_mode {
        let before = total_hits(&writer.masker.counts());
        let report = catch_up_closed_hook_segments(
            &hook_sources.ready,
            data_dir,
            &mut cursors,
            &mut writer,
        )?;
        discovered += report.files + report.skipped + report.invalid;
        let tally = Tally {
            files: report.files,
            events: report.events,
            skipped: report.skipped,
            failures: report.invalid,
            masked_hits: total_hits(&writer.masker.counts()) - before,
            ..Tally::default()
        };
        touched += report.files;
        per_runtime.insert(HOOKS.to_string(), serde_json::to_value(tally)?);
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
    let mut writer = Writer::new(data_dir.to_path_buf(), machine_name());
    let mut per_runtime: Map<String, Value> = Map::new();
    let mut tallies: HashMap<&'static str, Tally> = HashMap::new();
    let mut touched = 0u64;

    for path in paths {
        if selected.is_none() && hook_sources.segment_mode && path.starts_with(&hook_sources.ready)
        {
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
            if let Some(source) = &selected {
                adapter.runtime() == source.runtime && path.starts_with(&source.root)
            } else {
                adapter
                    .roots(&home)
                    .iter()
                    .any(|root| path.starts_with(root))
            }
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
            if cursor.is_current(&meta) {
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
