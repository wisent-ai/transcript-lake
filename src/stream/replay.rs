//! Explicit reconstruction retains its separate empty-destination contract.
use super::{stream_file, total_hits, warn, ReplayOptions, Tally, Writer};
use crate::cursors::{open_writer_lease, Cursors};
use crate::hook_segments::replay_closed_hook_segments;
use crate::types::{Adapter, HOOKS, SUPPORTED_SOURCES};
use crate::util::{home_dir, machine_name, Error, Result};
use serde_json::{json, Map, Value};
use std::fs;
use std::path::PathBuf;
use std::time::Instant;
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
                    match stream_file(
                        &mut writer,
                        &mut cursors,
                        adapter.as_ref(),
                        &entry,
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
