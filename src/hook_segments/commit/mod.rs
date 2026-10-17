//! Committing a segment: the cursor record it writes, the events it hands to
//! the sink, and the acknowledgement published beside it so the writer can
//! see that this segment was taken and what came of it.

use std::path::Path;

use serde_json::{json, Value};

use crate::cursors::{CursorRecord, Cursors};
use crate::types::EventSink;
use crate::util::{machine_name, Result};

use super::*;

mod claim;

use claim::{acquire_claim, release_claim};

/// What one pass over a segment did with it: it was not trustworthy (the
/// segment named), it was already committed or claimed elsewhere, or it
/// contributed this many events.
pub(super) enum Outcome {
    Invalid(PathBuf),
    Skipped,
    Committed(u64),
}

pub(super) fn publish_ack(ready_dir: &Path, segment: &Segment, commit: &Value) -> Result<()> {
    let path = acked_dir(ready_dir)?.join(ack_name(&segment.segment_id));
    // The acknowledgement names the commit and its outputs; a commit record
    // without them is a broken cursor, said with the record, never
    // acknowledged as a commit of nothing.
    let field = |key: &str| {
        commit.get(key).cloned().ok_or_else(|| {
            Error(format!(
                "hook segment {} commit record has no {key}: {commit}",
                segment.segment_id
            ))
        })
    };
    let ack = json!({
        "protocol": ACK_PROTOCOL,
        "segmentId": segment.segment_id,
        "sourceSha256": segment.source_sha256,
        "sourceSize": segment.source_size,
        "eventCount": segment.event_count,
        "payloadSha256": segment.payload_sha256,
        "lakeCommitId": field("commitId")?,
        "outputs": field("outputs")?,
    });
    let content = format!("{}\n", serde_json::to_string_pretty(&ack)?);
    if !acked(&path)? {
        return durable_write(&path, content.as_bytes());
    }
    // An acknowledgement that cannot be read is not a data conflict; saying
    // "conflict" sent the operator after a disagreement that never existed.
    let raw = fs::read_to_string(&path).map_err(|error| {
        Error(format!(
            "cannot read hook segment acknowledgement {}: {error}",
            path.display()
        ))
    })?;
    // A conflict is a disagreement about the DATA: a different source, a different
    // payload, a different event count, or different outputs. The commit id
    // identifies the process that wrote them, and one segment can be committed by
    // more than one process — a cursor restored from backup, a rebuild, or a
    // resumed stream. A differing process id alone is not a data conflict.
    // An acknowledgement that is not JSON is not a disagreement either.
    let prior = parse(&raw).ok_or_else(|| {
        Error(format!(
            "hook segment acknowledgement {} is not a JSON object",
            path.display()
        ))
    })?;
    let agrees = prior.get("sourceSha256") == ack.get("sourceSha256")
        && prior.get("payloadSha256") == ack.get("payloadSha256")
        && prior.get("eventCount") == ack.get("eventCount")
        && same_outputs(prior.get("outputs"), ack.get("outputs"));
    if !agrees {
        return Err(Error(format!(
            "hook segment acknowledgement conflict: {}",
            path.display()
        )));
    }
    if prior.get("lakeCommitId") == ack.get("lakeCommitId") {
        return Ok(());
    }
    // Re-point the acknowledgement at the commit the cursor now holds, so the two
    // records stop diverging on the next run.
    durable_write(&path, content.as_bytes())
}

pub(super) fn process_segment(
    path: &Path,
    data_dir: &Path,
    cursors: &mut Cursors,
    sink: &mut dyn EventSink,
) -> Result<Outcome> {
    let segment = match validate_hook_segment(path) {
        Ok(segment) => segment,
        Err(reason) => {
            warn(&format!(
                "invalid closed hook segment {}: {reason}",
                path.display()
            ));
            return Ok(Outcome::Invalid(path.to_path_buf()));
        }
    };
    let ready_dir = path.parent().ok_or_else(|| {
        Error(format!(
            "closed hook segment {} has no directory",
            path.display()
        ))
    })?;
    let key = format!("hooks:{}", segment.segment_id);
    if let Some(CursorRecord::Segment(existing)) = cursors.get(&key)? {
        let committed = existing.get("kind").and_then(Value::as_str) == Some(COMMIT_KIND)
            && existing.get("sourceSha256").and_then(Value::as_str)
                == Some(segment.source_sha256.as_str())
            && outputs_valid(existing.get("outputs"))?;
        if committed {
            publish_ack(ready_dir, &segment, &existing)?;
            return Ok(Outcome::Skipped);
        }
    }
    let claims_root = data_dir.join("staging").join("hooks").join("claims");
    let Some(claim) = acquire_claim(&claims_root, &segment.segment_id)? else {
        return Ok(Outcome::Skipped);
    };
    let outcome = commit_segment(path, &segment, ready_dir, cursors, sink);
    release_claim(&claim);
    outcome
}

pub(super) fn commit_segment(
    path: &Path,
    segment: &Segment,
    ready_dir: &Path,
    cursors: &mut Cursors,
    sink: &mut dyn EventSink,
) -> Result<Outcome> {
    let mut events = Vec::with_capacity(segment.events.len());
    for (sequence, record) in segment.events.iter().enumerate() {
        let event =
            map_canonical_hook_record(record, &segment.segment_id, &segment.created_at, sequence);
        // An unusable timestamp has no date partition to land in, which the previous
        // implementation rejected as an invalid canonical mapping rather than filing
        // under the catch-all.
        if event.ts.is_none() {
            warn(&format!(
                "closed hook segment produced an invalid canonical mapping: {}",
                segment.segment_id
            ));
            continue;
        }
        events.push(event);
    }
    if events.is_empty() {
        return Err(Error(
            "closed hook segment produced no canonical events".into(),
        ));
    }
    let published = sink.accept(path, &events)?;
    if published.is_empty() {
        return Err(Error(
            "closed hook segment produced no canonical events".into(),
        ));
    }
    // The acknowledgement and the cursor record carry exactly these two keys per
    // output; both are read back by the next run and by the rotator.
    let outputs: Vec<Value> = published
        .iter()
        .map(|output| {
            json!({
                "path": output.path.to_string_lossy(),
                "sha256": output.sha256,
            })
        })
        .collect();
    let commit = json!({
        "kind": COMMIT_KIND,
        "protocol": PROTOCOL,
        "state": "committed",
        "segmentId": segment.segment_id,
        "sourceSha256": segment.source_sha256,
        "sourceSize": segment.source_size,
        "eventCount": segment.event_count,
        "payloadSha256": segment.payload_sha256,
        "commitId": uuid::Uuid::new_v4().to_string(),
        "outputs": outputs,
    });
    cursors.set_segment(&format!("hooks:{}", segment.segment_id), commit.clone());
    cursors.flush()?;
    publish_ack(ready_dir, segment, &commit)?;
    Ok(Outcome::Committed(segment.events.len() as u64))
}
