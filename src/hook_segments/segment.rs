//! A closed segment as this reader is allowed to trust it: the footer digest
//! it claims, the file it was read from unchanged underneath, and the outputs
//! a prior commit published still being there with the content it recorded.

use std::fs;
use std::path::Path;

use serde_json::Value;

use super::*;

/// A validated closed segment: every field the commit and the acknowledgement quote.
pub(super) struct Segment {
    pub(super) segment_id: String,
    pub(super) created_at: Value,
    pub(super) source_sha256: String,
    pub(super) source_size: u64,
    pub(super) payload_sha256: String,
    pub(super) event_count: usize,
    pub(super) events: Vec<Value>,
}

/// A segment is trusted only when it is a plain regular file that has not changed
/// under us, is complete (header, in-sequence event frames, footer), and hashes to
/// the digest its own footer claims.
pub(super) fn validate_hook_segment(path: &Path) -> Option<Segment> {
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

pub(super) fn same_inode(before: &fs::Metadata, after: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    before.ino() == after.ino()
}

/// Every published output still exists with the content the commit recorded.
pub(super) fn outputs_valid(outputs: Option<&Value>) -> bool {
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

pub(super) fn output_key(item: &Value) -> String {
    format!(
        "{}\u{0}{}",
        string_field(item, "path").unwrap_or_default(),
        string_field(item, "sha256").unwrap_or_default()
    )
}

pub(super) fn same_outputs(prior: Option<&Value>, current: Option<&Value>) -> bool {
    let (Some(Value::Array(prior)), Some(Value::Array(current))) = (prior, current) else {
        return false;
    };
    if prior.len() != current.len() {
        return false;
    }
    let seen: Vec<String> = prior.iter().map(output_key).collect();
    current.iter().all(|item| seen.contains(&output_key(item)))
}

