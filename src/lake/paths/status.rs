//! The health of the cursor store and of the live stream, read for `status`
//! and `doctor` without changing either.
use std::fs;
use std::path::Path;

use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Serialize)]
pub struct CursorStatus {
    pub state: &'static str,
    /// Records in the cursor store; absent when no store could be read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub files: Option<usize>,
    #[serde(rename = "newestSourceMtime")]
    pub newest_source_mtime: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Cursor inventory for `status` and `doctor`; never mutates the store.
pub fn read_cursor_status(path: &Path) -> CursorStatus {
    let invalid = |error: String| CursorStatus {
        state: "invalid",
        files: None,
        newest_source_mtime: None,
        error: Some(error),
    };
    // Only a store that is not there is absent: one that cannot be read is
    // invalid with the cause, where Path::exists made it absent too.
    let raw = match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return CursorStatus {
                state: "absent",
                files: None,
                newest_source_mtime: None,
                error: None,
            }
        }
        Err(error) => return invalid(format!("{} cannot be read: {error}", path.display())),
    };
    let parsed: Value = match serde_json::from_str(&raw) {
        Ok(parsed) => parsed,
        Err(error) => return invalid(format!("{} is not JSON: {error}", path.display())),
    };
    let Some(store) = parsed.as_object() else {
        return invalid("cursor store is not an object".to_string());
    };
    let newest = store
        .values()
        .filter_map(|record| record.get("mtimeMs").and_then(Value::as_f64))
        .filter(|value| value.is_finite())
        .fold(f64::NEG_INFINITY, f64::max);
    // No finite stamp is no newest time; one outside the calendar is none too.
    let newest_source_mtime = crate::adapters::common::iso_epoch_millis(newest);
    CursorStatus {
        state: "healthy",
        files: Some(store.len()),
        newest_source_mtime,
        error: None,
    }
}

#[derive(Debug, Serialize)]
pub struct StreamStatus {
    pub state: &'static str,
    pub summary: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// The live stream's state for `status` and `doctor`. A stream that kept
/// `failed`, or that started again while carrying the failure of the start
/// before it (`lastFailure`, kept until a catch-up completes), is `failed`
/// with that error: a supervisor restarting it after every failure leaves it
/// failing, not catching up. A stream still `catching-up` whose recorded
/// process (pid and start time on this host) no longer runs never finished
/// its catch-up and advances no cursor: it is `failed` naming that process,
/// never shown as catching up; one whose process cannot be checked is
/// `failed` with why.
pub fn read_stream_status(path: &Path) -> StreamStatus {
    // Only a stream record that is not there is absent; one that cannot be
    // read is invalid with the cause.
    let raw = match fs::read_to_string(path) {
        Ok(raw) => Ok(raw),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return StreamStatus {
                state: "absent",
                summary: None,
                error: None,
            }
        }
        Err(error) => Err(format!("{} cannot be read: {error}", path.display())),
    };
    match raw.and_then(|raw| {
        serde_json::from_str::<Value>(&raw).map_err(|error| format!("{} is not JSON: {error}", path.display()))
    }) {
        Ok(summary) => {
            let failure = match summary["state"].as_str() {
                Some("failed") => summary["error"]
                    .as_str()
                    .map(|error| repeated(error, &summary["failedStarts"])),
                Some("catching-up") => abandoned(&summary).or_else(|| carried(&summary)),
                _ => carried(&summary),
            };
            match failure {
                Some(error) => StreamStatus {
                    state: "failed",
                    summary: Some(summary),
                    error: Some(error),
                },
                None => StreamStatus {
                    state: "healthy",
                    summary: Some(summary),
                    error: None,
                },
            }
        }
        Err(error) => StreamStatus {
            state: "invalid",
            summary: None,
            error: Some(error),
        },
    }
}

/// How many starts in a row have ended failed, this one included, from the
/// failure `carried` by the start before it. A record written before starts
/// were counted stands for at least the one failure it names.
pub fn failed_starts(carried: &Value) -> u64 {
    let first = std::num::NonZeroU64::MIN.get();
    match (carried.is_null(), carried["starts"].as_u64()) {
        (true, _) => first,
        (false, Some(before)) => before.saturating_add(first),
        (false, None) => first.saturating_add(first),
    }
}

/// The failure a start carries from the starts before it, with how many of
/// them ended that way.
fn carried(summary: &Value) -> Option<String> {
    let last = &summary["lastFailure"];
    last["error"].as_str().map(|error| repeated(error, &last["starts"]))
}

/// `error`, saying how many starts in a row ended with it when that was
/// counted: a supervisor restarting the stream makes each start fail alike.
fn repeated(error: &str, starts: &Value) -> String {
    match starts.as_u64() {
        Some(starts) => format!("{error} ({starts} starts in a row ended failed since the last completed catch-up)"),
        None => error.to_string(),
    }
}

/// Why a `catching-up` record stands for a catch-up nobody finishes: its
/// recorded process no longer runs, or it names none, or whether it runs
/// cannot be read. `None` while that process still runs.
fn abandoned(summary: &Value) -> Option<String> {
    let since = &summary["startedAt"];
    let process = &summary["process"];
    let (Some(host), Some(pid), Some(started)) =
        (process["host"].as_str(), process["pid"].as_u64(), process["started"].as_str())
    else {
        return Some(format!(
            "the stream records a catch-up started at {since} with no process (host, pid and start time) behind it, so whether it still runs cannot be told; restart the stream"
        ));
    };
    let pid = match u32::try_from(pid) {
        Ok(pid) => pid,
        Err(error) => return Some(format!("the stream's catch-up names process {pid}, which is no process id: {error}")),
    };
    match crate::lake::owner::alive(host, pid, started) {
        Ok(true) => None,
        Ok(false) => Some(format!(
            "the catch-up started at {since} by stream process {pid} on {host} never finished: that process no longer runs, so no cursor advances"
        )),
        Err(crate::util::Error(cause)) => Some(format!(
            "whether stream process {pid} on {host}, catching up since {since}, still runs cannot be read: {cause}"
        )),
    }
}
