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

const PROTOCOL: &str = "hooks-telemetry-segment-v1";
const ACK_PROTOCOL: &str = "hooks-telemetry-ack-v1";
const COMMIT_KIND: &str = "closed-segment";

/// What one pass over the ready directory consumed.
#[derive(Debug, Default, Clone, Copy)]
pub struct SegmentReport {
    pub files: u64,
    pub events: u64,
    pub skipped: u64,
    pub invalid: u64,
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn segment_name(id: &str) -> String {
    format!("segment-{id}.jsonl")
}

fn ack_name(id: &str) -> String {
    format!("segment-{id}.ack.json")
}

/// A JSON object, or nothing. Arrays and scalars are not records here.
fn parse(text: &str) -> Option<Value> {
    let value = serde_json::from_str::<Value>(text).ok()?;
    value.is_object().then_some(value)
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    match value.get(key) {
        Some(Value::String(text)) if !text.is_empty() => Some(text.clone()),
        _ => None,
    }
}

/// JS truthiness, for the `x || null` fallbacks the record mapping is built from.
fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number
            .as_f64()
            .is_some_and(|raw| raw != 0.0 && !raw.is_nan()),
        Value::String(text) => !text.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

/// `record[key] || null`.
fn or_null(record: &Value, key: &str) -> Value {
    match record.get(key) {
        Some(value) if truthy(value) => value.clone(),
        _ => Value::Null,
    }
}

/// `record[key] ?? null`.
fn nullish(record: &Value, key: &str) -> Value {
    match record.get(key) {
        Some(value) if !value.is_null() => value.clone(),
        _ => Value::Null,
    }
}

/// JS `Number(value)`, which the record mapping applies to the raw `ts` field.
fn js_number(value: Option<&Value>) -> f64 {
    match value {
        None => f64::NAN,
        Some(Value::Null) => 0.0,
        Some(Value::Bool(flag)) => {
            if *flag {
                1.0
            } else {
                0.0
            }
        }
        Some(Value::Number(number)) => number.as_f64().unwrap_or(f64::NAN),
        Some(Value::String(text)) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                0.0
            } else {
                trimmed.parse::<f64>().unwrap_or(f64::NAN)
            }
        }
        Some(Value::Array(items)) => match items.len() {
            0 => 0.0,
            1 => js_number(items.first()),
            _ => f64::NAN,
        },
        Some(Value::Object(_)) => f64::NAN,
    }
}

fn epoch_iso(millis: f64) -> Option<String> {
    if !millis.is_finite() {
        return None;
    }
    let stamp = chrono::DateTime::from_timestamp_millis(millis as i64)?;
    Some(stamp.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
}

/// One record inside a closed segment, as the canonical event the sink will mask,
/// canonicalize and write. Every `extra` key is always present, including the nulls,
/// because sql/views.sql and sql/signals.sql project them by name.
fn map_canonical_hook_record(
    record: &Value,
    segment_id: &str,
    segment_created_at: &Value,
    sequence: usize,
) -> RawEvent {
    let ts = epoch_iso(js_number(record.get("ts")));
    let mut extra = Map::new();
    extra.insert("source_type".into(), or_null(record, "type"));
    extra.insert("hook_id".into(), or_null(record, "id"));
    extra.insert("decision".into(), or_null(record, "decision"));
    extra.insert("code".into(), nullish(record, "code"));
    extra.insert(
        "timed_out".into(),
        Value::Bool(record.get("timedOut") == Some(&Value::Bool(true))),
    );
    extra.insert("infra".into(), or_null(record, "infra"));
    extra.insert("source".into(), or_null(record, "source"));
    extra.insert("episode_id".into(), or_null(record, "episode_id"));
    extra.insert(
        "adaptive_state_persisted".into(),
        Value::Bool(record.get("adaptiveStatePersisted") == Some(&Value::Bool(true))),
    );
    extra.insert(
        "causal_episode_persisted".into(),
        Value::Bool(record.get("causalEpisodePersisted") == Some(&Value::Bool(true))),
    );
    extra.insert("segment_created_at".into(), segment_created_at.clone());
    extra.insert("segment_id".into(), Value::from(segment_id));
    extra.insert("sequence".into(), Value::from(sequence));
    extra.insert("payload_ts".into(), nullish(record, "payloadTs"));
    extra.insert("payload".into(), nullish(record, "payload"));
    extra.insert("meta".into(), nullish(record, "meta"));
    extra.insert("label".into(), or_null(record, "label"));
    extra.insert("repair_kind".into(), or_null(record, "kind"));
    extra.insert("evidence".into(), or_null(record, "evidence"));
    let text = string_field(record, "reason")
        .or_else(|| string_field(record, "text"))
        .unwrap_or_default();
    RawEvent {
        ts,
        session_id: string_field(record, "session_id"),
        project: string_field(record, "project"),
        event_type: "hook_decision".to_string(),
        text,
        tool_name: string_field(record, "id"),
        model: None,
        tokens_in: None,
        tokens_out: None,
        extra,
    }
}

fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)?.sync_all()?;
    Ok(())
}

fn durable_write(path: &Path, content: &[u8]) -> Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let name = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let temporary = parent.join(format!(".{name}.{}.tmp", uuid::Uuid::new_v4()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    file.write_all(content)?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temporary, path)?;
    sync_directory(parent)?;
    Ok(())
}

/// A validated closed segment: every field the commit and the acknowledgement quote.
struct Segment {
    segment_id: String,
    created_at: Value,
    source_sha256: String,
    source_size: u64,
    payload_sha256: String,
    event_count: usize,
    events: Vec<Value>,
}

/// A segment is trusted only when it is a plain regular file that has not changed
/// under us, is complete (header, in-sequence event frames, footer), and hashes to
/// the digest its own footer claims.
fn validate_hook_segment(path: &Path) -> Option<Segment> {
    let before = fs::symlink_metadata(path).ok()?;
    if !before.is_file() || before.file_type().is_symlink() {
        return None;
    }
    let bytes = fs::read(path).ok()?;
    if bytes.is_empty() || bytes.last() != Some(&b'\n') {
        return None;
    }
    // A lossy decode would silently rewrite bytes the digest was taken over.
    let source = String::from_utf8(bytes.clone()).ok()?;
    let rows: Vec<&str> = source[..source.len() - 1].split('\n').collect();
    if rows.len() < 2 {
        return None;
    }
    let header = parse(rows[0])?;
    let footer = parse(rows[rows.len() - 1])?;
    if header.get("kind").and_then(Value::as_str) != Some("segment_open") {
        return None;
    }
    if header.get("protocol").and_then(Value::as_str) != Some(PROTOCOL) {
        return None;
    }
    let segment_id = string_field(&header, "segmentId")?;
    let created_at = header.get("createdAt").cloned()?;
    if !created_at.as_f64().is_some_and(f64::is_finite) {
        return None;
    }
    string_field(&header, "producerId")?;
    string_field(&header, "invocationId")?;
    let source_ok = header.get("source").is_some_and(|value| {
        truthy(value) && value.get("producer").and_then(Value::as_str) == Some("hooks-rotator")
    });
    if !source_ok {
        return None;
    }
    if path.file_name()?.to_string_lossy() != segment_name(&segment_id) {
        return None;
    }
    let mut events = Vec::new();
    for row in &rows[1..rows.len() - 1] {
        let frame = parse(row)?;
        if frame.get("kind").and_then(Value::as_str) != Some("event") {
            return None;
        }
        if frame.get("sequence").and_then(Value::as_f64) != Some(events.len() as f64) {
            return None;
        }
        let event = frame.get("event")?;
        if !event.is_object() {
            return None;
        }
        events.push(event.clone());
    }
    let payload = format!("{}\n", rows[..rows.len() - 1].join("\n"));
    let payload_sha256 = digest(payload.as_bytes());
    if footer.get("kind").and_then(Value::as_str) != Some("segment_close") {
        return None;
    }
    if footer.get("protocol").and_then(Value::as_str) != Some(PROTOCOL) {
        return None;
    }
    if footer.get("segmentId").and_then(Value::as_str) != Some(segment_id.as_str()) {
        return None;
    }
    if footer.get("eventCount").and_then(Value::as_f64) != Some(events.len() as f64) {
        return None;
    }
    if footer.get("payloadSha256").and_then(Value::as_str) != Some(payload_sha256.as_str()) {
        return None;
    }
    let after = fs::symlink_metadata(path).ok()?;
    if !after.is_file() || after.len() != before.len() || !same_inode(&before, &after) {
        return None;
    }
    Some(Segment {
        segment_id,
        created_at,
        source_sha256: digest(&bytes),
        source_size: bytes.len() as u64,
        payload_sha256,
        event_count: events.len(),
        events,
    })
}

fn same_inode(before: &fs::Metadata, after: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    before.ino() == after.ino()
}

/// Every published output still exists with the content the commit recorded.
fn outputs_valid(outputs: Option<&Value>) -> bool {
    let Some(Value::Array(items)) = outputs else {
        return false;
    };
    if items.is_empty() {
        return false;
    }
    items.iter().all(|item| {
        let (Some(path), Some(sha256)) = (string_field(item, "path"), string_field(item, "sha256"))
        else {
            return false;
        };
        let path = PathBuf::from(path);
        if !path.exists() {
            return false;
        }
        fs::read(&path).is_ok_and(|bytes| digest(&bytes) == sha256)
    })
}

fn output_key(item: &Value) -> String {
    format!(
        "{}\u{0}{}",
        string_field(item, "path").unwrap_or_default(),
        string_field(item, "sha256").unwrap_or_default()
    )
}

fn same_outputs(prior: Option<&Value>, current: Option<&Value>) -> bool {
    let (Some(Value::Array(prior)), Some(Value::Array(current))) = (prior, current) else {
        return false;
    };
    if prior.len() != current.len() {
        return false;
    }
    let seen: Vec<String> = prior.iter().map(output_key).collect();
    current.iter().all(|item| seen.contains(&output_key(item)))
}

fn publish_ack(ready_dir: &Path, segment: &Segment, commit: &Value) -> Result<()> {
    let acked_dir = ready_dir
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("acked");
    let path = acked_dir.join(ack_name(&segment.segment_id));
    let outputs = commit.get("outputs").cloned().unwrap_or(Value::Null);
    let ack = json!({
        "protocol": ACK_PROTOCOL,
        "segmentId": segment.segment_id,
        "sourceSha256": segment.source_sha256,
        "sourceSize": segment.source_size,
        "eventCount": segment.event_count,
        "payloadSha256": segment.payload_sha256,
        "lakeCommitId": commit.get("commitId").cloned().unwrap_or(Value::Null),
        "outputs": outputs,
    });
    let content = format!("{}\n", serde_json::to_string_pretty(&ack)?);
    if !path.exists() {
        return durable_write(&path, content.as_bytes());
    }
    let prior = fs::read_to_string(&path).ok().and_then(|raw| parse(&raw));
    // A conflict is a disagreement about the DATA: a different source, a different
    // payload, a different event count, or different outputs. The commit id
    // identifies the process that wrote them, and one segment can be committed by
    // more than one process — a cursor restored from backup, a rebuild, or a
    // resumed stream. A differing process id alone is not a data conflict.
    let agrees = prior.as_ref().is_some_and(|prior| {
        prior.get("sourceSha256") == ack.get("sourceSha256")
            && prior.get("payloadSha256") == ack.get("payloadSha256")
            && prior.get("eventCount") == ack.get("eventCount")
            && same_outputs(prior.get("outputs"), ack.get("outputs"))
    });
    if !agrees {
        return Err(Error(format!(
            "hook segment acknowledgement conflict: {}",
            path.display()
        )));
    }
    let prior = prior.unwrap_or(Value::Null);
    if prior.get("lakeCommitId") == ack.get("lakeCommitId") {
        return Ok(());
    }
    // Re-point the acknowledgement at the commit the cursor now holds, so the two
    // records stop diverging on the next run.
    durable_write(&path, content.as_bytes())
}

fn process_identity(pid: u32) -> Option<String> {
    let output = Command::new("/bin/ps")
        .args(["-o", "lstart=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// `kill(pid, 0)`: `Ok(())` when the signal was deliverable, `Err(true)` for EPERM
/// (the process exists but is not ours), `Err(false)` for ESRCH and anything else.
fn signal_zero(pid: i32) -> std::result::Result<(), bool> {
    extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
        fn __error() -> *mut i32;
    }
    // SAFETY: kill with signal zero performs an existence and permission check only.
    let (result, errno) = unsafe {
        let result = kill(pid, 0);
        (result, *__error())
    };
    if result == 0 {
        return Ok(());
    }
    // EPERM is 1 on Darwin: the process exists and belongs to another user.
    Err(errno == 1)
}

fn owner_alive(owner: Option<&Value>, segment_id: &str) -> bool {
    let Some(owner) = owner else {
        return false;
    };
    let Some(pid) = owner.get("pid").and_then(Value::as_i64) else {
        return false;
    };
    if let Some(host) = string_field(owner, "host") {
        if host != machine_name() {
            return true;
        }
    }
    let current_identity = process_identity(pid as u32);
    if let (Some(started), Some(current)) = (string_field(owner, "started"), &current_identity) {
        return &started == current;
    }
    match signal_zero(pid as i32) {
        Ok(()) => {
            warn(&format!(
                "hook segment claim identity unavailable; retaining live pid claim: {segment_id}"
            ));
            true
        }
        Err(exists) => exists,
    }
}

/// A per-segment claim so two concurrent runs never publish the same segment twice.
struct Claim {
    path: PathBuf,
    nonce: String,
}

fn acquire_claim(root: &Path, segment_id: &str) -> Result<Option<Claim>> {
    fs::create_dir_all(root)?;
    let path = root.join(format!("{segment_id}.claim"));
    for retry in [false, true] {
        let nonce = uuid::Uuid::new_v4().to_string();
        let owner = json!({
            "host": machine_name(),
            "pid": std::process::id(),
            "started": process_identity(std::process::id()),
            "nonce": nonce,
        });
        let temporary = root.join(format!(".{segment_id}.{nonce}.claim"));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(format!("{}\n", serde_json::to_string(&owner)?).as_bytes())?;
        file.sync_all()?;
        drop(file);
        match fs::hard_link(&temporary, &path) {
            Ok(()) => {
                let _ = fs::remove_file(&temporary);
                sync_directory(root)?;
                return Ok(Some(Claim { path, nonce }));
            }
            Err(error) => {
                let _ = fs::remove_file(&temporary);
                if error.kind() != std::io::ErrorKind::AlreadyExists {
                    return Err(error.into());
                }
                let incumbent = fs::read_to_string(&path).ok().and_then(|raw| parse(&raw));
                if owner_alive(incumbent.as_ref(), segment_id) {
                    warn(&format!("hook segment already claimed: {segment_id}"));
                    return Ok(None);
                }
                if fs::remove_file(&path).is_err() || sync_directory(root).is_err() {
                    return Ok(None);
                }
                if retry {
                    return Ok(None);
                }
            }
        }
    }
    Ok(None)
}

fn release_claim(claim: &Claim) {
    let owner = fs::read_to_string(&claim.path)
        .ok()
        .and_then(|raw| parse(&raw));
    let held = owner
        .as_ref()
        .and_then(|owner| string_field(owner, "nonce"));
    if held.as_deref() != Some(claim.nonce.as_str()) {
        return;
    }
    if fs::remove_file(&claim.path).is_ok() {
        if let Some(parent) = claim.path.parent() {
            let _ = sync_directory(parent);
        }
    }
}

enum Outcome {
    Invalid,
    Skipped,
    Committed(u64),
}

fn process_segment(
    path: &Path,
    data_dir: &Path,
    cursors: &mut Cursors,
    sink: &mut dyn EventSink,
) -> Result<Outcome> {
    let Some(segment) = validate_hook_segment(path) else {
        warn(&format!("invalid closed hook segment: {}", path.display()));
        return Ok(Outcome::Invalid);
    };
    let ready_dir = path.parent().unwrap_or_else(|| Path::new("."));
    let key = format!("hooks:{}", segment.segment_id);
    if let Some(CursorRecord::Segment(existing)) = cursors.get(&key)? {
        let committed = existing.get("kind").and_then(Value::as_str) == Some(COMMIT_KIND)
            && existing.get("sourceSha256").and_then(Value::as_str)
                == Some(segment.source_sha256.as_str())
            && outputs_valid(existing.get("outputs"));
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

fn commit_segment(
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
pub fn hooks_adapter() -> Box<dyn Adapter> {
    Box::new(Hooks)
}

const PICK: [&str; 7] = [
    "event", "decision", "tool", "code", "ms", "timedOut", "infra",
];

struct Hooks;

impl Adapter for Hooks {
    fn runtime(&self) -> &'static str {
        crate::types::HOOKS
    }

    fn roots(&self, home: &Path) -> Vec<PathBuf> {
        let dir = home.join(".hooks-adaptive");
        if dir.exists() {
            return vec![dir];
        }
        Vec::new()
    }

    fn list_sessions(&self, root: &Path) -> Vec<SessionEntry> {
        let mut out = Vec::new();
        for name in ["telemetry.prev.jsonl", "telemetry.jsonl"] {
            let file = root.join(name);
            if file.exists() {
                out.push(SessionEntry {
                    file,
                    session_id: None,
                    project: None,
                });
            }
        }
        out
    }

    /// The two telemetry logs this adapter reads, recognised by name. Anything
    /// else under the root — a closed segment, a claim, an acknowledgement —
    /// belongs to the segment path, not to this one.
    fn entry_for(&self, path: &Path) -> Option<SessionEntry> {
        let name = path.file_name()?.to_str()?;
        if !matches!(name, "telemetry.prev.jsonl" | "telemetry.jsonl") {
            return None;
        }
        let root = crate::util::home_dir().join(".hooks-adaptive");
        if path.parent() != Some(root.as_path()) {
            return None;
        }
        Some(SessionEntry {
            file: path.to_path_buf(),
            session_id: None,
            project: None,
        })
    }

    fn parser(&self, ctx: ParserCtx) -> Box<dyn Parser> {
        Box::new(HooksParser { file: ctx.file })
    }
}

struct HooksParser {
    file: PathBuf,
}

impl Parser for HooksParser {
    /// The frozen parser interface maps a malformed line to zero events; the drop is
    /// still reported on stderr so it is never invisible.
    fn on_line(&mut self, raw: &str) -> Vec<RawEvent> {
        let line = raw.trim();
        if line.is_empty() {
            return Vec::new();
        }
        let rec = match serde_json::from_str::<Value>(line) {
            Ok(value) => value,
            Err(error) => {
                warn(&format!(
                    "{}: dropped malformed telemetry line: {error}",
                    self.file.display()
                ));
                return Vec::new();
            }
        };
        if !rec.is_object() && !rec.is_array() {
            return Vec::new();
        }
        let ts = match rec.get("ts") {
            Some(Value::Number(number)) => {
                let millis = number.as_f64().unwrap_or(f64::NAN);
                if !millis.is_finite() {
                    return Vec::new();
                }
                // `new Date(ms).toISOString()` threw a RangeError here, which the
                // driver caught per line and reported; the line is still dropped.
                match epoch_iso(millis) {
                    Some(stamp) => Some(stamp),
                    None => {
                        warn(&format!(
                            "{}: dropped telemetry line with an unrepresentable timestamp",
                            self.file.display()
                        ));
                        return Vec::new();
                    }
                }
            }
            Some(Value::String(text)) => Some(text.clone()),
            _ => None,
        };
        let Some(ts) = ts.filter(|value| !value.is_empty()) else {
            return Vec::new();
        };
        let mut extra = Map::new();
        for key in PICK {
            if let Some(value) = rec.get(key) {
                if !value.is_null() {
                    extra.insert(key.to_string(), value.clone());
                }
            }
        }
        vec![RawEvent {
            ts: Some(ts),
            session_id: first_present(&rec, &["session_id", "sessionId", "session"]),
            project: first_present(&rec, &["project", "cwd"]),
            event_type: "hook_decision".to_string(),
            text: match rec.get("reason") {
                Some(Value::String(reason)) => reason.clone(),
                _ => String::new(),
            },
            tool_name: match rec.get("id") {
                Some(Value::String(id)) => Some(id.clone()),
                _ => None,
            },
            model: None,
            tokens_in: None,
            tokens_out: None,
            extra,
        }]
    }
}

/// The `a ?? b ?? c` chain: the first key that is present and not null wins. A
/// non-string winner is rendered rather than dropped, because session_id is the join
/// key every downstream label and export is grouped by.
fn first_present(rec: &Value, keys: &[&str]) -> Option<String> {
    for key in keys {
        let Some(value) = rec.get(*key) else {
            continue;
        };
        if value.is_null() {
            continue;
        }
        return Some(match value.as_str() {
            Some(text) => text.to_string(),
            None => value.to_string(),
        });
    }
    None
}
