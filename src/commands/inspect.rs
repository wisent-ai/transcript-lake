//! Discovery and health commands: resolved paths, source availability,
//! dependency checks, and the Lake status snapshot. Everything here reads;
//! neither the state root nor any source store is modified. These four
//! commands are the ones an operator runs before trusting the Lake, so their
//! exit status is meaningful: a broken cursor store, an unreadable last-ingest
//! summary, or a source that cannot be enumerated is a non-zero exit.
use std::fs;
use std::io::Write;
use std::path::PathBuf;

use serde::Serialize;
use serde_json::Value;

use crate::args::{parse_options, require_flags_only};
use crate::paths::{
    hook_source_roots, lake_paths, partition_report, read_cursor_status, read_last_ingest,
    CursorStatus, LastIngest, PartitionRow,
};
use crate::types::HOOKS;
use crate::util::{home_dir, write_json, Result};

/// JavaScript `String(value)` for one JSON field: the raw text of a string,
/// the literal spelling of a number or boolean, `null`, and `undefined` for a
/// key the summary never carried. The status lines quote these verbatim, so
/// the rendering has to survive a summary written by an older version.
pub fn js_string(value: Option<&Value>) -> String {
    match value {
        None => "undefined".to_string(),
        Some(Value::Null) => "null".to_string(),
        Some(Value::String(text)) => text.clone(),
        Some(other) => other.to_string(),
    }
}

pub fn paths(rest: &[String]) -> Result<i32> {
    let parsed = parse_options("paths", rest, &[], &["json"])?;
    require_flags_only("paths", &parsed)?;
    let report = lake_paths();
    if parsed.flag("json") {
        write_json(&report)?;
        return Ok(0);
    }
    // The human listing is the JSON object printed one key per line, so the
    // two views cannot drift: an added path shows up in both at once.
    let Value::Object(entries) = serde_json::to_value(&report)? else {
        unreachable!("lake paths always serialize to an object");
    };
    let mut out = std::io::stdout().lock();
    for (name, value) in entries {
        let text = value
            .as_str()
            .filter(|text| !text.is_empty())
            .unwrap_or("not found");
        writeln!(out, "{name}: {text}")?;
    }
    Ok(0)
}

/// One supported source store as `sources` and `doctor` report it.
#[derive(Debug, Serialize)]
pub struct SourceRow {
    pub runtime: String,
    pub available: bool,
    pub mode: &'static str,
    pub roots: Vec<String>,
    pub files: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

fn display_roots(roots: &[PathBuf]) -> Vec<String> {
    roots
        .iter()
        .map(|root| root.to_string_lossy().into_owned())
        .collect()
}

/// Adaptive-hook telemetry is not a transcript adapter: it arrives either as
/// closed segments under the ready directory or as the legacy mutable log, and
/// the file count means a different thing in each mode.
fn hooks_row() -> SourceRow {
    let hooks = hook_source_roots();
    let files = if hooks.segment_mode {
        match fs::read_dir(&hooks.ready) {
            Ok(entries) => entries
                .flatten()
                .filter(|entry| {
                    entry.file_type().map(|kind| kind.is_file()).unwrap_or(false)
                        && entry.file_name().to_string_lossy().ends_with(".jsonl")
                })
                .count() as u64,
            Err(error) => {
                return SourceRow {
                    runtime: HOOKS.to_string(),
                    available: false,
                    mode: "error",
                    roots: Vec::new(),
                    files: 0,
                    error: Some(error.to_string()),
                }
            }
        }
    } else if hooks.legacy.exists() {
        ["telemetry.prev.jsonl", "telemetry.jsonl"]
            .iter()
            .filter(|name| hooks.legacy.join(name).exists())
            .count() as u64
    } else {
        0
    };
    SourceRow {
        runtime: HOOKS.to_string(),
        available: hooks.available,
        mode: if hooks.segment_mode {
            "closed-segments"
        } else {
            "legacy-log"
        },
        roots: display_roots(&hooks.roots),
        files,
        error: None,
    }
}

/// Availability and candidate-file counts for every supported source, in the
/// order ingest walks them: every transcript adapter, then hooks.
pub fn source_report() -> Vec<SourceRow> {
    let home = home_dir();
    let mut rows = Vec::new();
    for adapter in crate::adapters::all() {
        let roots = adapter.roots(&home);
        let files = roots
            .iter()
            .map(|root| adapter.list_sessions(root).len() as u64)
            .sum();
        rows.push(SourceRow {
            runtime: adapter.runtime().to_string(),
            available: !roots.is_empty(),
            mode: "transcripts",
            roots: display_roots(&roots),
            files,
            error: None,
        });
    }
    rows.push(hooks_row());
    rows
}

pub fn sources(rest: &[String]) -> Result<i32> {
    let parsed = parse_options("sources", rest, &[], &["json"])?;
    require_flags_only("sources", &parsed)?;
    let rows = source_report();
    let status = i32::from(rows.iter().any(|row| row.error.is_some()));
    if parsed.flag("json") {
        write_json(&rows)?;
        return Ok(status);
    }
    let mut out = std::io::stdout().lock();
    for row in &rows {
        let state = if row.available { row.mode } else { "not found" };
        let suffix = match &row.error {
            Some(error) => format!(" error={error}"),
            None => String::new(),
        };
        writeln!(out, "{}: {state}, {} files{suffix}", row.runtime, row.files)?;
        for root in &row.roots {
            writeln!(out, "  {root}")?;
        }
    }
    Ok(status)
}

#[derive(Debug, Serialize)]
pub struct StatusReport {
    #[serde(rename = "dataDir")]
    pub data_dir: PathBuf,
    pub partitions: Vec<PartitionRow>,
    pub cursors: CursorStatus,
    #[serde(rename = "lastIngest")]
    pub last_ingest: LastIngest,
    pub oko: Value,
}

/// Everything `status` prints, gathered without touching DuckDB: partition
/// inventory, cursor health, last ingest, and Oko export freshness. Oko is
/// optional, so an exporter that cannot answer reports an `unavailable` state
/// rather than failing the whole snapshot.
pub fn status_snapshot() -> StatusReport {
    let paths = lake_paths();
    StatusReport {
        partitions: partition_report(&paths.data_dir),
        cursors: read_cursor_status(&paths.cursors),
        last_ingest: read_last_ingest(&paths.last_ingest),
        oko: crate::oko_export::freshness(),
        data_dir: paths.data_dir,
    }
}

pub fn status(rest: &[String]) -> Result<i32> {
    let parsed = parse_options("status", rest, &[], &["json"])?;
    require_flags_only("status", &parsed)?;
    let report = status_snapshot();
    let status =
        i32::from(report.cursors.state == "invalid" || report.last_ingest.state == "invalid");
    if parsed.flag("json") {
        write_json(&report)?;
        return Ok(status);
    }
    let mut out = std::io::stdout().lock();
    writeln!(out, "data dir: {}", report.data_dir.display())?;
    if report.partitions.is_empty() {
        writeln!(out, "partitions: none (run ingest first)")?;
    }
    for row in &report.partitions {
        writeln!(
            out,
            "  {}: {} partition files, {} bytes",
            row.runtime, row.parts, row.bytes
        )?;
    }
    let newest = match &report.cursors.newest_source_mtime {
        Some(stamp) => format!(", newest {stamp}"),
        None => String::new(),
    };
    writeln!(
        out,
        "cursors: {}, {} tracked files{newest}",
        report.cursors.state, report.cursors.files
    )?;
    if let Some(error) = &report.cursors.error {
        writeln!(out, "  cursor error: {error}")?;
    }
    // A summary of literal `null` parses cleanly but carries nothing, so the
    // state is reported instead of a line of placeholders.
    let summary = report
        .last_ingest
        .summary
        .as_ref()
        .filter(|value| !value.is_null());
    match summary {
        Some(last) => {
            let failures = match last.get("failures") {
                Some(Value::Null) | None => "0".to_string(),
                other => js_string(other),
            };
            writeln!(
                out,
                "last ingest: {}, failures {failures}",
                js_string(last.get("finishedAt"))
            )?
        }
        None => writeln!(out, "last ingest: {}", report.last_ingest.state)?,
    }
    if let Some(error) = &report.last_ingest.error {
        writeln!(out, "  summary error: {error}")?;
    }
    match &report.oko {
        Value::String(text) => writeln!(out, "oko: {text}")?,
        other => writeln!(out, "oko: {}", serde_json::to_string(other)?)?,
    }
    Ok(status)
}

#[derive(Debug, Serialize)]
struct Check {
    name: &'static str,
    status: &'static str,
    detail: String,
}

#[derive(Debug, Serialize)]
struct DoctorReport {
    #[serde(rename = "dataDir")]
    data_dir: PathBuf,
    healthy: bool,
    checks: Vec<Check>,
}

pub fn doctor(rest: &[String]) -> Result<i32> {
    let parsed = parse_options("doctor", rest, &[], &["json"])?;
    require_flags_only("doctor", &parsed)?;
    let paths = lake_paths();
    let cursors = read_cursor_status(&paths.cursors);
    let sources = source_report();
    let broken: Vec<&SourceRow> = sources.iter().filter(|row| row.error.is_some()).collect();
    let checks = vec![
        Check {
            name: "state-root",
            // An absent state root is the zero state, not a fault: the first
            // ingest creates it.
            status: "ok",
            detail: if paths.data_dir.exists() {
                paths.data_dir.to_string_lossy().into_owned()
            } else {
                format!("absent zero-state: {}", paths.data_dir.display())
            },
        },
        Check {
            name: "cursors",
            status: if cursors.state == "invalid" { "error" } else { "ok" },
            detail: match &cursors.error {
                Some(error) => format!("{}: {error}", cursors.state),
                None => cursors.state.to_string(),
            },
        },
        Check {
            name: "sources",
            status: if sources.iter().any(|row| row.available) {
                "ok"
            } else {
                "warning"
            },
            detail: format!(
                "{} supported runtimes found",
                sources.iter().filter(|row| row.available).count()
            ),
        },
        Check {
            name: "source-integrity",
            status: if broken.is_empty() { "ok" } else { "error" },
            detail: if broken.is_empty() {
                "all installed adapters loaded".to_string()
            } else {
                broken
                    .iter()
                    .map(|row| {
                        format!("{}: {}", row.runtime, row.error.as_deref().unwrap_or_default())
                    })
                    .collect::<Vec<String>>()
                    .join("; ")
            },
        },
        Check {
            name: "duckdb",
            status: if paths.duckdb.is_some() { "ok" } else { "warning" },
            detail: match &paths.duckdb {
                Some(found) => found.to_string_lossy().into_owned(),
                None => {
                    "optional dependency not found; analytics and compact unavailable".to_string()
                }
            },
        },
        Check {
            name: "oko-cli",
            status: if paths.oko_cli.is_some() { "ok" } else { "warning" },
            detail: match &paths.oko_cli {
                Some(found) => found.to_string_lossy().into_owned(),
                None => "optional dependency not found; reindex unavailable".to_string(),
            },
        },
    ];
    let report = DoctorReport {
        data_dir: paths.data_dir,
        healthy: !checks.iter().any(|check| check.status == "error"),
        checks,
    };
    if parsed.flag("json") {
        write_json(&report)?;
    } else {
        let mut out = std::io::stdout().lock();
        for check in &report.checks {
            writeln!(
                out,
                "{} {}: {}",
                check.status.to_uppercase(),
                check.name,
                check.detail
            )?;
        }
    }
    Ok(i32::from(!report.healthy))
}
