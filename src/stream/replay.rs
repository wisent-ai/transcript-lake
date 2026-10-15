//! Explicit reconstruction retains its separate empty-destination contract.
use super::{stream_file, total_hits, warn, ReplayOptions, Tally, Writer};
use crate::cursors::{open_writer_lease, Cursors};
use crate::hook_segments::replay_closed_hook_segments;
use crate::types::{supported_sources, Adapter, HOOKS};
use crate::util::{home_dir, machine_name, Error, Result};
use serde_json::{json, Map, Value};
use std::fs;
use std::time::Instant;

fn replay_locked(opts: &ReplayOptions) -> Result<Value> {
    let started = Instant::now();
    let data_dir = opts.data_dir.clone();
    let machine = machine_name();
    let requested = opts.source.as_deref();
    let supported = supported_sources();
    if let Some(name) = requested {
        if !supported.contains(&name) {
            return Err(Error(format!(
                "unknown source \"{name}\" (expected one of: {}; `transcript-lake sources` shows each with its roots on this machine)",
                supported.join(", ")
            )));
        }
    }
    let selected: Vec<&str> = match requested {
        Some(name) => vec![name],
        None => supported.clone(),
    };
    let mut cursors = Cursors::open(&data_dir)?;
    let mut writer = Writer::new(data_dir.clone(), machine)?;
    let home = home_dir();
    let mut per_runtime = Map::new();
    for name in selected {
        let mut tally = Tally::default();
        let before = total_hits(&writer.masker.counts());
        let mut adapter: Option<Box<dyn Adapter>> = None;
        if name == HOOKS {
            let ready = crate::lake::paths::hook_segments_ready();
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
                tally.failures += std::num::NonZeroU64::MIN.get();
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
                        Ok(()) => tally.files += std::num::NonZeroU64::MIN.get(),
                        Err(error) => {
                            warn(&format!("{}: {error}", entry.file.display()));
                            tally.failures += std::num::NonZeroU64::MIN.get();
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
        "partial": std::num::NonZeroU64::new(failures).is_some(),
        "failures": failures,
    }))
}

/// Replay every selected source into a separate empty Lake for recovery.
pub fn replay(opts: ReplayOptions) -> Result<Value> {
    // An absent root is empty; one that cannot be listed is refused, since a
    // rebuild into a root nobody could inspect might duplicate what it holds.
    let occupied = match fs::read_dir(&opts.data_dir) {
        Ok(mut entries) => entries.next().is_some(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => {
            return Err(Error(format!(
                "rebuild cannot tell whether {} is empty: {error}",
                opts.data_dir.display()
            )))
        }
    };
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
