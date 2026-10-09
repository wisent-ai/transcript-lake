//! What every harness adapter needs and none writes twice: directory listings
//! in byte order, the type of a path a notification named, JSON documents and
//! JSON-line files read whole, read-only SQLite, and times from the epoch
//! numbers harnesses record. Event construction is in `events`.
//!
//! A directory that vanished mid-scan yields nothing, as every adapter's scan
//! contract says; a session that is read and cannot be is an error naming it.
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::types::SessionEntry;
use crate::util::{Error, Result};

mod events;

pub(crate) use events::*;

/// The editor products that ship the VS Code extension host, by the directory
/// name each gives its user data (VS Code's documented layout).
const VS_CODE_EDITORS: &[&str] = &[
    "Code",
    "Code - Insiders",
    "VSCodium",
    "Cursor",
    "Windsurf",
    "Kiro",
    "Trae",
    "Antigravity",
];

/// Every entry of `dir` with its type, in byte order of the name; an
/// unreadable or vanished directory has none.
pub(crate) fn read_dirents(dir: &Path) -> Vec<(String, fs::FileType)> {
    let Ok(iter) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in iter.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        out.push((entry.file_name().to_string_lossy().to_string(), file_type));
    }
    out.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    out
}

/// The type of a path the caller already knows, without following a symlink,
/// so an entry a scan skips is skipped here too.
pub(crate) fn dirent_type(path: &Path) -> Option<fs::FileType> {
    fs::symlink_metadata(path).ok().map(|meta| meta.file_type())
}

/// Whether `path` is a regular file (not a symlink, not a directory).
pub(crate) fn is_file(path: &Path) -> bool {
    dirent_type(path).is_some_and(|kind| kind.is_file())
}

/// The directories directly inside `dir`, in byte order.
pub(crate) fn subdirectories(dir: &Path) -> Vec<PathBuf> {
    read_dirents(dir)
        .into_iter()
        .filter(|(_, kind)| kind.is_dir())
        .map(|(name, _)| dir.join(name))
        .collect()
}

/// The regular files directly inside `dir` whose name ends with `suffix`.
pub(crate) fn files_with_suffix(dir: &Path, suffix: &str) -> Vec<PathBuf> {
    read_dirents(dir)
        .into_iter()
        .filter(|(name, kind)| kind.is_file() && name.ends_with(suffix))
        .map(|(name, _)| dir.join(name))
        .collect()
}

/// `dir` when it is a directory: the one root a harness keeps all its
/// sessions under.
pub(crate) fn existing_root(dir: PathBuf) -> Vec<PathBuf> {
    if dir.is_dir() {
        vec![dir]
    } else {
        Vec::new()
    }
}

/// The name of `path`'s file, as text.
pub(crate) fn file_name(path: &Path) -> Option<String> {
    Some(path.file_name()?.to_string_lossy().to_string())
}

/// The name of `path`'s parent directory, as text.
pub(crate) fn parent_name(path: &Path) -> Option<String> {
    file_name(path.parent()?)
}

/// One session entry.
pub(crate) fn entry(
    file: PathBuf,
    session_id: Option<String>,
    project: Option<String>,
) -> SessionEntry {
    SessionEntry {
        file,
        session_id,
        project,
    }
}

/// Whether `path` is inside one of `roots` (the roots an adapter declared).
pub(crate) fn under_any(path: &Path, roots: &[PathBuf]) -> bool {
    roots.iter().any(|root| path.starts_with(root))
}

/// The user data directory of every VS Code-family editor present: VS Code's
/// documented per-platform location (`~/Library/Application
/// Support/<Editor>/User` on macOS, `~/.config/<Editor>/User` elsewhere).
pub(crate) fn editor_user_dirs(home: &Path) -> Vec<PathBuf> {
    let bases = [
        home.join("Library").join("Application Support"),
        home.join(".config"),
    ];
    let mut dirs = Vec::new();
    for base in bases {
        for editor in VS_CODE_EDITORS {
            let user = base.join(editor).join("User");
            if user.is_dir() {
                dirs.push(user);
            }
        }
    }
    dirs
}

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

/// Every complete JSON line of a file read whole; a last line still being
/// written (no newline yet) is left for the next read, and a line that is not
/// JSON carries no record.
pub(crate) fn read_json_lines(path: &Path) -> Result<Vec<Value>> {
    let text = fs::read_to_string(path)
        .map_err(|error| Error(format!("cannot read {}: {error}", path.display())))?;
    let complete = match text.rfind('\n') {
        Some(end) => &text[..end],
        None => "",
    };
    Ok(complete
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect())
}

/// A SQLite database opened read-only, so reading a harness's store never
/// takes a write lock it holds or creates a file it does not have.
pub(crate) fn open_database(path: &Path) -> Result<rusqlite::Connection> {
    rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|error| {
        Error(format!(
            "cannot open the database {} read-only: {error}",
            path.display()
        ))
    })
}

/// A database error, naming the database and what was asked of it.
pub(crate) fn database_error(path: &Path, asked: &str, error: rusqlite::Error) -> Error {
    Error(format!(
        "{} could not answer {asked}: {error}",
        path.display()
    ))
}

/// The path a SQLite notification names, mapped to its database: a write
/// lands in `<db>-wal` (or `<db>-journal`) before the database file.
pub(crate) fn database_of(path: &Path) -> PathBuf {
    let text = path.to_string_lossy();
    for companion in ["-wal", "-journal", "-shm"] {
        if let Some(database) = text.strip_suffix(companion) {
            return PathBuf::from(database);
        }
    }
    path.to_path_buf()
}

/// ISO-8601 text of a moment given in milliseconds since the epoch.
pub(crate) fn iso_millis(millis: i64) -> Option<String> {
    let stamp = chrono::DateTime::from_timestamp_millis(millis)?;
    Some(stamp.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
}

/// ISO-8601 text of a moment given in (possibly fractional) seconds since the
/// epoch; a negative or non-finite value is no moment.
pub(crate) fn iso_seconds(seconds: f64) -> Option<String> {
    let since = std::time::Duration::try_from_secs_f64(seconds).ok()?;
    let stamp: chrono::DateTime<chrono::Utc> = std::time::UNIX_EPOCH.checked_add(since)?.into();
    Some(stamp.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
}

/// A JSON number field holding epoch milliseconds, as ISO-8601 text.
pub(crate) fn iso_millis_field(value: &Value, key: &str) -> Option<String> {
    iso_millis(value.get(key)?.as_f64()? as i64)
}

/// A JSON string field, when present and non-empty.
pub(crate) fn text_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

/// A JSON integer field.
pub(crate) fn int_field(value: &Value, key: &str) -> Option<i64> {
    let number = value.get(key)?;
    number.as_i64().or_else(|| {
        number
            .as_f64()
            .filter(|raw| raw.is_finite())
            .map(|raw| raw as i64)
    })
}

/// The words a content value carries: a string as is, an array of strings or
/// of blocks with a `text` field joined by newlines.
pub(crate) fn text_of(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|item| match item {
                Value::String(text) => Some(text.clone()),
                Value::Object(map) => map.get("text").and_then(Value::as_str).map(str::to_string),
                _ => None,
            })
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// A tool input or output as the text an event carries: a string as is,
/// anything else as compact JSON.
pub(crate) fn as_text(value: Option<&Value>) -> String {
    match value {
        None | Some(Value::Null) => String::new(),
        Some(Value::String(text)) => text.clone(),
        Some(other) => other.to_string(),
    }
}
