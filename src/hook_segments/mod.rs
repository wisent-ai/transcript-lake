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

/// What one pass over the ready directory consumed. `invalid` counts the
/// segments `invalid_segments` names, so a report can say which they were.
#[derive(Debug, Default, Clone)]
pub struct SegmentReport {
    pub files: u64,
    pub events: u64,
    pub skipped: u64,
    pub invalid: u64,
    pub invalid_segments: Vec<PathBuf>,
}

impl SegmentReport {
    /// The report of a pass is a count of what each segment it took came to.
    fn of(outcomes: &[Outcome]) -> Self {
        let count = |kind: fn(&Outcome) -> bool| outcomes.iter().filter(|o| kind(o)).count() as u64;
        let invalid_segments: Vec<PathBuf> = outcomes
            .iter()
            .filter_map(|outcome| match outcome {
                Outcome::Invalid(path) => Some(path.clone()),
                Outcome::Committed(_) | Outcome::Skipped => None,
            })
            .collect();
        Self {
            files: count(|outcome| matches!(outcome, Outcome::Committed(_))),
            events: outcomes
                .iter()
                .filter_map(|outcome| match outcome {
                    Outcome::Committed(events) => Some(*events),
                    Outcome::Invalid(_) | Outcome::Skipped => None,
                })
                .sum(),
            skipped: count(|outcome| matches!(outcome, Outcome::Skipped)),
            invalid: invalid_segments.len() as u64,
            invalid_segments,
        }
    }
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
    let outcome = process_segment(path, data_dir, cursors, sink)?;
    Ok(SegmentReport::of(std::slice::from_ref(&outcome)))
}

/// The closed segments' directory; none when it does not exist yet. One that
/// cannot be listed is the error: read as empty, a replay or catch-up
/// reported nothing to do while every closed segment in it stayed unread.
fn ready_entries(ready_dir: &Path) -> Result<Option<fs::ReadDir>> {
    match fs::read_dir(ready_dir) {
        Ok(entries) => Ok(Some(entries)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(crate::util::Error(format!(
            "cannot list closed hook segments in {}: {error}",
            ready_dir.display()
        ))),
    }
}

/// The closed segments in the ready directory as (file name, segment id), in
/// stable filename order; none when the directory does not exist yet. An entry
/// that cannot be read is the error: skipped, the pass would report nothing
/// left to do while that segment stayed unread. A name that is not
/// `segment-<id>.jsonl` in UTF-8 names no segment.
fn closed_segments(ready_dir: &Path) -> Result<Option<Vec<(String, String)>>> {
    let Some(entries) = ready_entries(ready_dir)? else {
        return Ok(None);
    };
    let mut segments = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| {
            Error(format!(
                "cannot read an entry of the closed hook segments in {}: {error}",
                ready_dir.display()
            ))
        })?;
        let Ok(name) = entry.file_name().into_string() else {
            continue;
        };
        let Some(id) = name
            .strip_prefix("segment-")
            .and_then(|rest| rest.strip_suffix(".jsonl"))
        else {
            continue;
        };
        let id = id.to_string();
        segments.push((name, id));
    }
    segments.sort_unstable();
    Ok(Some(segments))
}

/// Where acknowledgements of the segments in `ready_dir` are published: the
/// `acked` directory beside it.
fn acked_dir(ready_dir: &Path) -> Result<PathBuf> {
    let parent = ready_dir.parent().ok_or_else(|| {
        Error(format!(
            "hook segment directory {} has no parent to hold acknowledgements",
            ready_dir.display()
        ))
    })?;
    Ok(parent.join("acked"))
}

/// Whether an acknowledgement is published. One whose existence cannot be
/// checked is the error, not an absence that sends the segment round again.
fn acked(path: &Path) -> Result<bool> {
    path.try_exists().map_err(|error| {
        Error(format!(
            "cannot check hook segment acknowledgement {}: {error}",
            path.display()
        ))
    })
}

/// Replay every closed segment in stable filename order.
pub fn replay_closed_hook_segments(
    ready_dir: &Path,
    data_dir: &Path,
    cursors: &mut Cursors,
    sink: &mut dyn EventSink,
) -> Result<SegmentReport> {
    let Some(segments) = closed_segments(ready_dir)? else {
        return Ok(SegmentReport::default());
    };
    let mut outcomes = Vec::new();
    for (name, _) in segments {
        outcomes.push(process_segment(
            &ready_dir.join(name),
            data_dir,
            cursors,
            sink,
        )?);
    }
    Ok(SegmentReport::of(&outcomes))
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
    let Some(segments) = closed_segments(ready_dir)? else {
        return Ok(SegmentReport::default());
    };
    let acked_dir = acked_dir(ready_dir)?;
    let mut outcomes = Vec::new();
    for (name, id) in segments {
        let committed = match cursors.get(&format!("hooks:{id}"))? {
            Some(CursorRecord::Segment(record)) => {
                record.get("kind").and_then(Value::as_str) == Some(COMMIT_KIND)
                    && outputs_valid(record.get("outputs"))?
                    && acked(&acked_dir.join(ack_name(&id)))?
            }
            _ => false,
        };
        if committed {
            outcomes.push(Outcome::Skipped);
            continue;
        }
        outcomes.push(process_segment(
            &ready_dir.join(name),
            data_dir,
            cursors,
            sink,
        )?);
    }
    Ok(SegmentReport::of(&outcomes))
}
