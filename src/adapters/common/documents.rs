//! Reading the files a harness writes: a whole JSON document, a JSON-line
//! transcript, and the sidecar files a session keeps beside its transcript.
//! A file the session is made of is an error naming it when it cannot be
//! read; a sidecar only adds what the transcript does not say, so a lost one
//! is said on stderr instead.

use std::fs;
use std::path::Path;

use serde_json::Value;

use crate::util::{Error, Result};

/// A whole JSON document; a file that cannot be read or parsed is an error
/// naming it.
pub(crate) fn read_json(path: &Path) -> Result<Value> {
    let bytes = fs::read(path)
        .map_err(|error| Error(format!("cannot read {}: {error}", path.display())))?;
    serde_json::from_slice(&bytes).map_err(|error| {
        Error(format!(
            "{} is not a JSON document: {error}",
            path.display()
        ))
    })
}

/// A file a session keeps beside its transcript (a header, a state, a
/// summary, a project root) that only adds what the transcript does not say.
/// One that is not there adds nothing; one that cannot be read, or is not the
/// text or JSON it should be, adds nothing either but is said on stderr with
/// its cause, as the scan says a directory it cannot list, so a session's
/// lost project or id has a reason.
pub(crate) fn sidecar(path: &Path) -> Option<Vec<u8>> {
    match fs::read(path) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            eprintln!("sessions: cannot read {}: {error}", path.display());
            None
        }
    }
}

/// A [`sidecar`] that holds text.
pub(crate) fn sidecar_text(path: &Path) -> Option<String> {
    match String::from_utf8(sidecar(path)?) {
        Ok(text) => Some(text),
        Err(error) => {
            eprintln!("sessions: {} is not text: {error}", path.display());
            None
        }
    }
}

/// A [`sidecar`] that holds one JSON document.
pub(crate) fn sidecar_json(path: &Path) -> Option<Value> {
    match serde_json::from_slice(&sidecar(path)?) {
        Ok(document) => Some(document),
        Err(error) => {
            eprintln!(
                "sessions: {} is not a JSON document: {error}",
                path.display()
            );
            None
        }
    }
}

/// Every complete JSON line of a file read whole; a last line still being
/// written (no newline yet) is left for the next read. A complete line that
/// is not JSON carries no record and is said on stderr with where it starts -
/// it used to vanish, so a damaged transcript read as a shorter one.
pub(crate) fn read_json_lines(path: &Path) -> Result<Vec<Value>> {
    let text = fs::read_to_string(path)
        .map_err(|error| Error(format!("cannot read {}: {error}", path.display())))?;
    let complete = match text.rfind('\n') {
        Some(end) => &text[..end],
        None => "",
    };
    let mut records = Vec::new();
    for line in complete.lines().filter(|line| !line.trim().is_empty()) {
        match serde_json::from_str(line) {
            Ok(record) => records.push(record),
            Err(error) => eprintln!(
                "sessions: the line at byte {} of {} is not JSON and carries no record: {error}",
                line.as_ptr() as usize - complete.as_ptr() as usize,
                path.display()
            ),
        }
    }
    Ok(records)
}
