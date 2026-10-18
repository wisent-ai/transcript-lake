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
/// the digest its own footer claims. Anything else is refused with the reason,
/// and a segment whose metadata or bytes cannot be read says the I/O cause, so
/// it is not taken for a malformed one.
pub(super) fn validate_hook_segment(path: &Path) -> std::result::Result<Segment, String> {
    let unreadable = |error: std::io::Error| format!("cannot be read: {error}");
    let before = fs::symlink_metadata(path).map_err(unreadable)?;
    if !before.is_file() || before.file_type().is_symlink() {
        return Err("is not a plain regular file".into());
    }
    let bytes = fs::read(path).map_err(unreadable)?;
    // A lossy decode would silently rewrite bytes the digest was taken over.
    let source =
        String::from_utf8(bytes.clone()).map_err(|error| format!("is not UTF-8: {error}"))?;
    let body = source
        .strip_suffix('\n')
        .ok_or("does not end with a newline, so its last frame may still be written")?;
    let rows: Vec<&str> = body.split('\n').collect();
    let [opening, frames @ .., closing] = rows.as_slice() else {
        return Err("holds no footer after its header".into());
    };
    let header = parse(opening).map_err(|cause| format!("header {cause}"))?;
    let footer = parse(closing).map_err(|cause| format!("footer {cause}"))?;
    if header.get("kind").and_then(Value::as_str) != Some("segment_open") {
        return Err("header is not a segment_open frame".into());
    }
    if header.get("protocol").and_then(Value::as_str) != Some(PROTOCOL) {
        return Err(format!("header does not speak protocol {PROTOCOL}"));
    }
    let segment_id = string_field(&header, "segmentId").ok_or("header has no segmentId")?;
    let created_at = header
        .get("createdAt")
        .filter(|value| value.as_f64().is_some_and(f64::is_finite))
        .cloned()
        .ok_or("header has no finite createdAt")?;
    string_field(&header, "producerId").ok_or("header has no producerId")?;
    string_field(&header, "invocationId").ok_or("header has no invocationId")?;
    let source_ok = header
        .get("source")
        .and_then(|source| source.get("producer"))
        .and_then(Value::as_str)
        == Some("hooks-rotator");
    if !source_ok {
        return Err("header source is not the hooks-rotator producer".into());
    }
    let expected = segment_name(&segment_id);
    if path.file_name().map(|name| name.to_string_lossy()) != Some(expected.as_str().into()) {
        return Err(format!("is not named {expected} after its segmentId"));
    }
    let mut events = Vec::new();
    for row in frames {
        let frame = parse(row).map_err(|cause| format!("frame {} {cause}", events.len()))?;
        if frame.get("kind").and_then(Value::as_str) != Some("event") {
            return Err(format!("frame {} is not an event frame", events.len()));
        }
        if frame.get("sequence").and_then(Value::as_f64) != Some(events.len() as f64) {
            return Err(format!("frame {} is out of sequence", events.len()));
        }
        let event = frame
            .get("event")
            .filter(|event| event.is_object())
            .ok_or_else(|| format!("frame {} carries no event object", events.len()))?;
        events.push(event.clone());
    }
    // The digest covers every row before the footer, each ending in a newline.
    let payload = body
        .strip_suffix(*closing)
        .ok_or("footer is not the segment's last row")?;
    let payload_sha256 = digest(payload.as_bytes());
    if footer.get("kind").and_then(Value::as_str) != Some("segment_close") {
        return Err("footer is not a segment_close frame".into());
    }
    if footer.get("protocol").and_then(Value::as_str) != Some(PROTOCOL) {
        return Err(format!("footer does not speak protocol {PROTOCOL}"));
    }
    if footer.get("segmentId").and_then(Value::as_str) != Some(segment_id.as_str()) {
        return Err("footer names another segmentId than its header".into());
    }
    if footer.get("eventCount").and_then(Value::as_f64) != Some(events.len() as f64) {
        return Err(format!(
            "footer eventCount does not match its {} event frames",
            events.len()
        ));
    }
    if footer.get("payloadSha256").and_then(Value::as_str) != Some(payload_sha256.as_str()) {
        return Err(format!(
            "footer payloadSha256 does not match its payload digest {payload_sha256}"
        ));
    }
    let after = fs::symlink_metadata(path).map_err(unreadable)?;
    if !after.is_file() || after.len() != before.len() || !same_inode(&before, &after) {
        return Err("changed while it was read".into());
    }
    Ok(Segment {
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
/// A record that names no output, an output that is gone and one whose content
/// changed are not valid, so the segment is committed again; an output that
/// exists but cannot be read is the error, since committing again would meet
/// the same failure without saying it.
pub(super) fn outputs_valid(outputs: Option<&Value>) -> Result<bool> {
    let Some(Value::Array(items)) = outputs else {
        return Ok(false);
    };
    if items.is_empty() {
        return Ok(false);
    }
    for item in items {
        let (Some(path), Some(sha256)) = (string_field(item, "path"), string_field(item, "sha256"))
        else {
            return Ok(false);
        };
        match fs::read(&path) {
            Ok(bytes) if digest(&bytes) == sha256 => {}
            Ok(_) => return Ok(false),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => {
                return Err(Error(format!(
                    "cannot read published hook segment output {path}: {error}"
                )))
            }
        }
    }
    Ok(true)
}

/// An output is identified by its path and digest together; one that lacks
/// either names nothing a later pass could find, so it has no key.
pub(super) fn output_key(item: &Value) -> Option<String> {
    Some(format!(
        "{}\u{0}{}",
        string_field(item, "path")?,
        string_field(item, "sha256")?
    ))
}

/// Two commits published the same outputs only when every output of each has a
/// key and the keys match one for one.
pub(super) fn same_outputs(prior: Option<&Value>, current: Option<&Value>) -> bool {
    let (Some(Value::Array(prior)), Some(Value::Array(current))) = (prior, current) else {
        return false;
    };
    if prior.len() != current.len() {
        return false;
    }
    let Some(seen) = prior
        .iter()
        .map(output_key)
        .collect::<Option<Vec<String>>>()
    else {
        return false;
    };
    current
        .iter()
        .all(|item| output_key(item).is_some_and(|key| seen.contains(&key)))
}
