//! Product state paths and cheap inspections: partition inventory, sizes and
//! where hook segments are read; cursor and stream health are read in
//! `status`.
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::util::{absolute, find_on_path, home_dir, Result};

mod status;

pub use status::{failed_starts, read_cursor_status, read_stream_status, CursorStatus, StreamStatus};

pub const STREAM_STATUS_FILE: &str = "stream-status.json";

/// Resolved state root: the explicit selection, `LAKE_DATA`, or the default.
pub fn resolve_data_dir(selected: Option<&Path>) -> PathBuf {
    if let Some(selected) = selected {
        return absolute(selected);
    }
    if let Some(from_env) = env::var_os("LAKE_DATA") {
        let path = PathBuf::from(from_env);
        if !path.as_os_str().is_empty() {
            return absolute(path);
        }
    }
    home_dir().join(".transcript-lake")
}

/// Whether anything is at `path`. Only an absent path is `false`; one that
/// cannot be examined is the error naming it and the cause, so a state file,
/// override or source root this process may not look at is never read as
/// absent and replaced by a default, a fresh start or the zero state.
pub fn present(path: &Path) -> Result<bool> {
    match fs::metadata(path) {
        Ok(_found) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!("{} cannot be examined: {error}", path.display()).into()),
    }
}

#[derive(Debug, Serialize)]
pub struct LakePaths {
    #[serde(rename = "dataDir")]
    pub data_dir: PathBuf,
    pub events: PathBuf,
    pub cursors: PathBuf,
    #[serde(rename = "sourceRegistry")]
    pub source_registry: PathBuf,
    #[serde(rename = "streamStatus")]
    pub stream_status: PathBuf,
    pub parquet: PathBuf,
    #[serde(rename = "okoExport")]
    pub oko_export: PathBuf,
    #[serde(rename = "okoStaging")]
    pub oko_staging: PathBuf,
    #[serde(rename = "hookSegments")]
    pub hook_segments: PathBuf,
    pub duckdb: Option<PathBuf>,
    #[serde(rename = "okoCli")]
    pub oko_cli: Option<PathBuf>,
}

/// Where closed hook segments are published for pickup:
/// `HOOKS_ADAPTIVE_SEGMENTS_READY` when it names a path, else the hooks'
/// own `~/.hooks-adaptive/telemetry-segments/ready`.
pub(crate) fn hook_segments_ready() -> PathBuf {
    match env::var_os("HOOKS_ADAPTIVE_SEGMENTS_READY").filter(|path| !path.is_empty()) {
        Some(path) => PathBuf::from(path),
        None => home_dir()
            .join(".hooks-adaptive")
            .join("telemetry-segments")
            .join("ready"),
    }
}

/// The Oko CLI every caller hands work to: `OKO_CLI` when it names a path,
/// else `oko-cli` on PATH, else none. One resolver, so `paths`, `doctor`,
/// the projection refresh and the reconcile hand-off cannot disagree about
/// which program is Oko.
pub(crate) fn oko_cli() -> Option<PathBuf> {
    match env::var_os("OKO_CLI").filter(|path| !path.is_empty()) {
        Some(path) => Some(PathBuf::from(path)),
        None => find_on_path("oko-cli"),
    }
}

pub fn lake_paths() -> LakePaths {
    let data_dir = resolve_data_dir(None);
    LakePaths {
        events: data_dir.join("events"),
        cursors: data_dir.join("cursors.json"),
        stream_status: data_dir.join(STREAM_STATUS_FILE),
        source_registry: data_dir.join(crate::sources::SOURCE_REGISTRY_FILE),
        parquet: data_dir.join("parquet"),
        oko_export: data_dir.join("exports").join("oko"),
        oko_staging: data_dir.join("staging").join("oko-export"),
        hook_segments: hook_segments_ready(),
        duckdb: find_on_path("duckdb"),
        oko_cli: oko_cli(),
        data_dir,
    }
}

#[derive(Debug, Serialize)]
pub struct PartitionRow {
    pub runtime: String,
    pub parts: u64,
    pub bytes: u64,
    /// Directories, entries and files under this runtime that could not be
    /// read: what they hold is missing from `parts` and `bytes`.
    pub unreadable: Vec<String>,
}

/// Per-runtime partition file counts and byte totals, sorted by runtime. No
/// events directory is no partitions. One that cannot be listed is refused
/// naming it: answered as no partitions, `compact` said "start the stream
/// first" about a lake full of events. A runtime or date directory, or a
/// part, that cannot be read is named in its row, so a short count is never
/// read as the whole lake.
pub fn partition_report(data_dir: &Path) -> Result<Vec<PartitionRow>> {
    let events_dir = data_dir.join("events");
    let mut rows = Vec::new();
    let unlisted = |error: std::io::Error| {
        crate::util::Error(format!(
            "partitions: cannot list {}, so no partition can be counted: {error}",
            events_dir.display()
        ))
    };
    let runtimes = match fs::read_dir(&events_dir) {
        Ok(runtimes) => runtimes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(rows),
        Err(error) => return Err(unlisted(error)),
    };
    for runtime_entry in runtimes {
        let runtime_entry = runtime_entry.map_err(unlisted)?;
        let name = runtime_entry.file_name().to_string_lossy().to_string();
        if !name.starts_with("runtime=") {
            continue;
        }
        // A runtime directory that cannot be examined is refused: skipped,
        // the inventory reported the lake without that runtime's partitions.
        let runtime_path = runtime_entry.path();
        if !present(&runtime_path)? || !runtime_path.is_dir() {
            continue;
        }
        let mut sizes = Vec::new();
        let mut unreadable = Vec::new();
        for date in entries_of(&runtime_entry.path(), &mut unreadable) {
            // A date directory that cannot be examined is named among the
            // unreadable, as an unlistable one is; skipped, its parts were
            // left out of the sizes without a word.
            match fs::metadata(&date) {
                Ok(meta) if meta.is_dir() => {}
                Ok(_not_directory) => continue,
                Err(error) => {
                    unreadable.push(format!("{}: {error}", date.display()));
                    continue;
                }
            }
            for file in entries_of(&date, &mut unreadable) {
                let part = file
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().ends_with(".ndjson"));
                if !part || !file.is_file() {
                    continue;
                }
                match fs::symlink_metadata(&file) {
                    Ok(meta) => sizes.push(meta.len()),
                    Err(error) => unreadable.push(format!("{}: {error}", file.display())),
                }
            }
        }
        rows.push(PartitionRow {
            runtime: name.trim_start_matches("runtime=").to_string(),
            parts: sizes.len() as u64,
            bytes: sizes.iter().sum(),
            unreadable,
        });
    }
    rows.sort_by(|left, right| left.runtime.cmp(&right.runtime));
    Ok(rows)
}

/// The paths `directory` lists. The directory, or an entry of it, that
/// cannot be read is named in `unreadable` instead of being skipped unsaid.
fn entries_of(directory: &Path, unreadable: &mut Vec<String>) -> Vec<PathBuf> {
    match fs::read_dir(directory) {
        Ok(entries) => entries
            .filter_map(|entry| match entry {
                Ok(entry) => Some(entry.path()),
                Err(error) => {
                    unreadable.push(format!("{}: {error}", directory.display()));
                    None
                }
            })
            .collect(),
        Err(error) => {
            unreadable.push(format!("{}: {error}", directory.display()));
            Vec::new()
        }
    }
}

/// Recursive byte size of a path, following no symlinks: the sum of what is
/// there, so a path that is not there holds no bytes. A path whose metadata
/// cannot be read, or a directory that cannot be listed, is the error, not a
/// size of nothing.
pub fn path_size(path: &Path) -> Result<u64> {
    let mut sizes = Vec::new();
    match fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => {
            let unlisted = |error: std::io::Error| {
                crate::util::Error(format!("cannot list {}: {error}", path.display()))
            };
            for entry in fs::read_dir(path).map_err(unlisted)? {
                sizes.push(path_size(&entry.map_err(unlisted)?.path())?);
            }
        }
        Ok(meta) => sizes.push(meta.len()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(crate::util::Error(format!(
                "cannot read {}: {error}",
                path.display()
            )))
        }
    }
    Ok(sizes.iter().sum())
}

/// Where the adaptive-hook segment reader looks, and whether it is populated.
pub struct HookSourceRoots {
    pub ready: PathBuf,
    pub legacy: PathBuf,
    pub segment_mode: bool,
    pub available: bool,
    pub roots: Vec<PathBuf>,
}

pub fn hook_source_roots() -> HookSourceRoots {
    let ready = hook_segments_ready();
    let legacy = home_dir().join(".hooks-adaptive");
    let segment_mode = ready.exists();
    let roots = if segment_mode {
        vec![ready.clone()]
    } else if legacy.exists() {
        vec![legacy.clone()]
    } else {
        Vec::new()
    };
    HookSourceRoots {
        available: segment_mode || legacy.exists(),
        ready,
        legacy,
        segment_mode,
        roots,
    }
}
