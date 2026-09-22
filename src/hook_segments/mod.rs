//! Transactional streaming for immutable adaptive-hook telemetry segments,
//! plus the legacy mutable-log pseudo-adapter used when no ready directory
//! exists.
//!
//! New hook outputs are deterministic per segment; acknowledgements publish last.
//! Segment reading, validation, claiming and the cursor commit live here; masking,
//! canonicalization and partition writing belong to the `EventSink` the driver
//! passes in, so there is exactly one place that writes events.
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::cursors::{CursorRecord, Cursors};
use crate::stream::warn;
use crate::types::{Adapter, EventSink, Parser, ParserCtx, RawEvent, SessionEntry};
use crate::util::{machine_name, Error, Result};

pub(super) const PROTOCOL: &str = "hooks-telemetry-segment-v1";
pub(super) const ACK_PROTOCOL: &str = "hooks-telemetry-ack-v1";
pub(super) const COMMIT_KIND: &str = "closed-segment";

/// What one pass over the ready directory consumed.
#[derive(Debug, Default, Clone, Copy)]
pub struct SegmentReport {
    pub files: u64,
    pub events: u64,
    pub skipped: u64,
    pub invalid: u64,
}


mod adapter;

mod commit;
mod reading;
mod segment;

pub use adapter::hooks_adapter;


use commit::{process_segment, Outcome};
use reading::*;
use segment::*;

/// Stream one immutable segment named by a filesystem notification.
pub fn stream_hook_segment(
    path: &Path,
    data_dir: &Path,
    cursors: &mut Cursors,
    sink: &mut dyn EventSink,
) -> Result<SegmentReport> {
    if !path
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("segment-") && name.ends_with(".jsonl"))
    {
        return Ok(SegmentReport::default());
    }
    let mut report = SegmentReport::default();
    match process_segment(path, data_dir, cursors, sink)? {
        Outcome::Invalid => report.invalid = 1,
        Outcome::Skipped => report.skipped = 1,
        Outcome::Committed(events) => {
            report.files = 1;
            report.events = events;
        }
    }
    Ok(report)
}

/// Replay every closed segment in stable filename order.
pub fn replay_closed_hook_segments(
    ready_dir: &Path,
    data_dir: &Path,
    cursors: &mut Cursors,
    sink: &mut dyn EventSink,
) -> Result<SegmentReport> {
    let Ok(entries) = fs::read_dir(ready_dir) else {
        return Ok(SegmentReport::default());
    };
    let mut names: Vec<String> = entries
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .filter(|name| name.starts_with("segment-") && name.ends_with(".jsonl"))
        .collect();
    names.sort_unstable();
    let mut report = SegmentReport::default();
    for name in names {
        match process_segment(&ready_dir.join(name), data_dir, cursors, sink)? {
            Outcome::Invalid => report.invalid += 1,
            Outcome::Skipped => report.skipped += 1,
            Outcome::Committed(events) => {
                report.files += 1;
                report.events += events;
            }
        }
    }
    Ok(report)
}

/// Resume only closed segments that have no durable Lake commit and producer
/// acknowledgement. A committed, acknowledged segment is immutable; explicit
/// recovery replay remains the path that revalidates every historical payload.
pub fn catch_up_closed_hook_segments(
    ready_dir: &Path,
    data_dir: &Path,
    cursors: &mut Cursors,
    sink: &mut dyn EventSink,
) -> Result<SegmentReport> {
    let Ok(entries) = fs::read_dir(ready_dir) else {
        return Ok(SegmentReport::default());
    };
    let acked_dir = ready_dir
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("acked");
    let mut names: Vec<String> = entries
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .filter(|name| name.starts_with("segment-") && name.ends_with(".jsonl"))
        .collect();
    names.sort_unstable();

    let mut report = SegmentReport::default();
    for name in names {
        let id = name
            .strip_prefix("segment-")
            .and_then(|value| value.strip_suffix(".jsonl"))
            .unwrap_or_default();
        let committed = match cursors.get(&format!("hooks:{id}"))? {
            Some(CursorRecord::Segment(record)) => {
                record.get("kind").and_then(Value::as_str) == Some(COMMIT_KIND)
                    && outputs_valid(record.get("outputs"))
                    && acked_dir.join(ack_name(id)).exists()
            }
            _ => false,
        };
        if committed {
            report.skipped += 1;
            continue;
        }
        match process_segment(&ready_dir.join(name), data_dir, cursors, sink)? {
            Outcome::Invalid => report.invalid += 1,
            Outcome::Skipped => report.skipped += 1,
            Outcome::Committed(events) => {
                report.files += 1;
                report.events += events;
            }
        }
    }
    Ok(report)
}

/// Pseudo-adapter over the adaptive hook decision log, used when no closed-segment
/// ready directory exists. Record shape, from the hooks-rotator telemetry writer:
/// `{ ts (epoch millis), event, id, decision, ms, code, tool, timedOut, infra, reason }`.
/// Downstream SQL relies on `extra.decision` / `extra.event` / `extra.infra` passing
/// through unchanged.
