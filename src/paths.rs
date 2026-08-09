//! Where every piece of product state lives, and the cheap inspections built
//! on top of it: partition inventory, cursor health, last-ingest summary.
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::Value;

use crate::util::{absolute, find_on_path, home_dir, Result};

pub const SUMMARY_FILE: &str = "last-ingest.json";

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

#[derive(Debug, Serialize)]
pub struct LakePaths {
    #[serde(rename = "dataDir")]
    pub data_dir: PathBuf,
    pub events: PathBuf,
    pub cursors: PathBuf,
    #[serde(rename = "lastIngest")]
    pub last_ingest: PathBuf,
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

pub fn lake_paths() -> LakePaths {
    let data_dir = resolve_data_dir(None);
    LakePaths {
        events: data_dir.join("events"),
        cursors: data_dir.join("cursors.json"),
        last_ingest: data_dir.join(SUMMARY_FILE),
        parquet: data_dir.join("parquet"),
        oko_export: data_dir.join("exports").join("oko"),
        oko_staging: data_dir.join("staging").join("oko-export"),
        hook_segments: env::var_os("HOOKS_ADAPTIVE_SEGMENTS_READY")
            .map(PathBuf::from)
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| {
                home_dir().join(".hooks-adaptive").join("telemetry-segments").join("ready")
            }),
        duckdb: find_on_path("duckdb"),
        oko_cli: env::var_os("OKO_CLI")
            .map(PathBuf::from)
            .filter(|path| !path.as_os_str().is_empty())
            .or_else(|| find_on_path("oko-cli")),
        data_dir,
    }
}

#[derive(Debug, Serialize)]
pub struct PartitionRow {
    pub runtime: String,
    pub parts: u64,
    pub bytes: u64,
}

/// Per-runtime partition file counts and byte totals, sorted by runtime.
pub fn partition_report(data_dir: &Path) -> Vec<PartitionRow> {
    let events_dir = data_dir.join("events");
    let mut rows = Vec::new();
    let Ok(runtimes) = fs::read_dir(&events_dir) else {
        return rows;
    };
    for runtime_entry in runtimes.flatten() {
        let name = runtime_entry.file_name().to_string_lossy().to_string();
        if !runtime_entry.path().is_dir() || !name.starts_with("runtime=") {
            continue;
        }
        let mut parts = 0u64;
        let mut bytes = 0u64;
        if let Ok(dates) = fs::read_dir(runtime_entry.path()) {
            for date_entry in dates.flatten() {
                if !date_entry.path().is_dir() {
                    continue;
                }
                let Ok(files) = fs::read_dir(date_entry.path()) else {
                    continue;
                };
                for file in files.flatten() {
                    let file_name = file.file_name().to_string_lossy().to_string();
                    if !file_name.ends_with(".ndjson") || !file.path().is_file() {
                        continue;
                    }
                    if let Ok(meta) = file.metadata() {
                        bytes += meta.len();
                    }
                    parts += 1;
                }
            }
        }
        rows.push(PartitionRow {
            runtime: name.trim_start_matches("runtime=").to_string(),
            parts,
            bytes,
        });
    }
    rows.sort_by(|left, right| left.runtime.cmp(&right.runtime));
    rows
}

#[derive(Debug, Serialize)]
pub struct CursorStatus {
    pub state: &'static str,
    pub files: usize,
    #[serde(rename = "newestSourceMtime")]
    pub newest_source_mtime: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Cursor inventory for `status` and `doctor`; never mutates the store.
pub fn read_cursor_status(path: &Path) -> CursorStatus {
    if !path.exists() {
        return CursorStatus {
            state: "absent",
            files: 0,
            newest_source_mtime: None,
            error: None,
        };
    }
    let invalid = |error: String| CursorStatus {
        state: "invalid",
        files: 0,
        newest_source_mtime: None,
        error: Some(error),
    };
    let raw = match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) => return invalid(error.to_string()),
    };
    let parsed: Value = match serde_json::from_str(&raw) {
        Ok(parsed) => parsed,
        Err(error) => return invalid(error.to_string()),
    };
    let Some(store) = parsed.as_object() else {
        return invalid("cursor store is not an object".to_string());
    };
    let newest = store
        .values()
        .filter_map(|record| record.get("mtimeMs").and_then(Value::as_f64))
        .filter(|value| value.is_finite())
        .fold(f64::NEG_INFINITY, f64::max);
    let newest_source_mtime = (newest.is_finite() && newest > f64::NEG_INFINITY).then(|| {
        chrono::DateTime::from_timestamp_millis(newest as i64)
            .map(|stamp| stamp.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
            .unwrap_or_default()
    });
    CursorStatus {
        state: "healthy",
        files: store.len(),
        newest_source_mtime,
        error: None,
    }
}

#[derive(Debug, Serialize)]
pub struct LastIngest {
    pub state: &'static str,
    pub summary: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub fn read_last_ingest(path: &Path) -> LastIngest {
    if !path.exists() {
        return LastIngest { state: "absent", summary: None, error: None };
    }
    match fs::read_to_string(path).map_err(|e| e.to_string()).and_then(|raw| {
        serde_json::from_str::<Value>(&raw).map_err(|error| error.to_string())
    }) {
        Ok(summary) => LastIngest { state: "healthy", summary: Some(summary), error: None },
        Err(error) => LastIngest { state: "invalid", summary: None, error: Some(error) },
    }
}

/// Recursive byte size of a path, following no symlinks.
pub fn path_size(path: &Path) -> Result<u64> {
    let Ok(meta) = fs::symlink_metadata(path) else {
        return Ok(0);
    };
    if !meta.is_dir() {
        return Ok(meta.len());
    }
    let mut total = 0;
    for entry in fs::read_dir(path)?.flatten() {
        total += path_size(&entry.path())?;
    }
    Ok(total)
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
    let ready = lake_paths().hook_segments;
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
