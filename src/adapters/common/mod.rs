//! What every harness adapter needs and none writes twice: directory listings
//! in byte order, the type of a path a notification named, and read-only
//! SQLite. JSON documents, JSON-line files and sidecars are read in
//! `documents`, events built in `events`, record fields and epoch times read
//! in `fields`, token usage carried in `usage`.
//!
//! A directory that vanished mid-scan yields nothing, as every adapter's scan
//! contract says; a session that is read and cannot be is an error naming it.
use std::fs;
use std::path::{Path, PathBuf};

use crate::types::SessionEntry;
use crate::util::{Error, Result};

mod documents;
mod events;
mod fields;
mod usage;

pub(crate) use documents::*;
pub(crate) use events::*;
pub(crate) use fields::*;
pub(crate) use usage::Usage;

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

/// Every entry of `dir` with its type, in byte order of the name. A vanished
/// directory has none; one that cannot be listed, and an entry whose type
/// cannot be read, is said on stderr with its cause, so the sessions under it
/// are not read as never having existed.
pub(crate) fn read_dirents(dir: &Path) -> Vec<(String, fs::FileType)> {
    let iter = match fs::read_dir(dir) {
        Ok(iter) => iter,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Vec::new(),
        Err(error) => {
            eprintln!("sessions: cannot list {}: {error}", dir.display());
            return Vec::new();
        }
    };
    let mut out = Vec::new();
    for entry in iter {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                eprintln!(
                    "sessions: cannot read an entry of {}: {error}",
                    dir.display()
                );
                continue;
            }
        };
        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(error) => {
                eprintln!(
                    "sessions: cannot read the type of {}: {error}",
                    entry.path().display()
                );
                continue;
            }
        };
        out.push((entry.file_name().to_string_lossy().to_string(), file_type));
    }
    out.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    out
}

/// The type of a path the caller already knows, without following a symlink,
/// so an entry a scan skips is skipped here too. A path that vanished has no
/// type; one whose type cannot be read is said on stderr with its cause, as
/// the scan says an entry it cannot read, rather than skipped in silence.
pub(crate) fn dirent_type(path: &Path) -> Option<fs::FileType> {
    match fs::symlink_metadata(path) {
        Ok(meta) => Some(meta.file_type()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            eprintln!(
                "sessions: cannot read the type of {}: {error}",
                path.display()
            );
            None
        }
    }
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

/// The working directory a harness encoded into a directory name by turning
/// each '/' into '-'. The reverse is best effort, since a dash that belonged
/// to the real path cannot be told apart, so a parser prefers the cwd a record
/// carries whenever it has one. A name that does not start with '-' encodes
/// no absolute path.
pub(crate) fn decode_dashed_path(name: &str) -> Option<String> {
    if !name.starts_with('-') {
        return None;
    }
    Some(name.replace('-', "/"))
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
