//! Materialize every masked conversation runtime as canonical per-session JSONL
//! under LAKE_DATA/exports/oko. Oko imports this stable view and no longer
//! parses vendor transcript stores for its catalog, search, or statistics.
//!
//! Normal runs track each append-only partition by size and mtime, merge only
//! new rows into affected sessions, deduplicate by a deterministic event UUID,
//! and preserve unchanged file mtimes. A first run, explicit --full, partition
//! truncation, or same-size rewrite rebuilds from all Lake partitions through
//! bounded staging buffers. Session writes and cursor publication are atomic.
//!
//! This was one file of 1157 lines until 2026-09-12, and no edit could touch
//! it: a source file over three hundred lines is refused here, so the export
//! had become unmaintainable by the rule that guards every other file. The
//! parts are unchanged and now sit where they belong - partition discovery in
//! `partitions`, row shaping in `row`, per-session writes in `sessions`, the
//! run and its cursor bookkeeping in `run`, and the read-only comparison of
//! Oko's index with the lake cursors in `run::freshness`.

mod partitions;
mod row;
mod run;
mod sessions;

use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::util::{home_dir, Result};

pub(crate) use row::fingerprint;
pub use run::export_oko_with_reindex;
pub use run::freshness::freshness;
pub(crate) use run::project_events;

pub(crate) fn oko_support_dir() -> PathBuf {
    home_dir()
        .join("Library")
        .join("Application Support")
        .join("Oko")
}

/// The Oko transcript index this machine would read. Shared with the DuckDB
/// bridge, which has to substitute it into the signal views because ATTACH
/// takes a literal path and does not expand a tilde.
pub fn oko_index_path() -> PathBuf {
    oko_support_dir().join("transcript-index.sqlite")
}

#[derive(Default)]
pub(crate) struct Tally {
    pub(crate) malformed: u64,
    pub(crate) last_error: Option<String>,
}

pub(crate) fn session_key(runtime: &str, session_id: &str) -> String {
    format!("{runtime}\n{session_id}")
}

pub(crate) fn hash_text(text: &str) -> String {
    format!("{:x}", Sha256::digest(text.as_bytes()))
}

pub(crate) fn atomic_write(file: &Path, content: &str) -> Result<()> {
    if let Some(parent) = file.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = PathBuf::from(format!("{}.tmp-{}", file.display(), std::process::id()));
    fs::write(&temporary, content)?;
    fs::rename(&temporary, file)?;
    Ok(())
}

pub(crate) fn remove_tree(path: &Path) -> Result<()> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn read_text(file: &Path) -> Option<String> {
    fs::read(file)
        .ok()
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
}
