//! What `status` reports without touching DuckDB: the partition inventory,
//! the cursor health, the live stream state, and Oko freshness beside them.

use std::io::Write;
use std::path::PathBuf;

use serde::Serialize;
use serde_json::Value;

use crate::args::{parse_options, require_flags_only};
use crate::paths::{
    lake_paths, partition_report, read_cursor_status, read_stream_status, CursorStatus,
    PartitionRow, StreamStatus,
};
use crate::util::{write_json, Result};

use super::js_string;

#[derive(Debug, Serialize)]
pub struct StatusReport {
    #[serde(rename = "dataDir")]
    pub data_dir: PathBuf,
    #[serde(rename = "selectedSource", skip_serializing_if = "Option::is_none")]
    pub selected_source: Option<crate::sources::AdoptedSource>,
    pub partitions: Vec<PartitionRow>,
    pub cursors: CursorStatus,
    pub stream: StreamStatus,
    pub oko: Value,
}

/// Everything `status` prints without touching DuckDB: partition inventory,
/// cursor health, live stream state, and Oko freshness. Oko is optional.
pub fn status_snapshot() -> Result<StatusReport> {
    let paths = lake_paths();
    Ok(StatusReport {
        selected_source: crate::sources::selected_source(&paths.data_dir)?,
        partitions: partition_report(&paths.data_dir)?,
        cursors: read_cursor_status(&paths.cursors),
        stream: read_stream_status(&paths.stream_status),
        oko: crate::oko_export::freshness(),
        data_dir: paths.data_dir,
    })
}

pub fn status(rest: &[String]) -> Result<i32> {
    let parsed = parse_options("status", rest, &[], &["json"])?;
    require_flags_only("status", &parsed)?;
    let report = status_snapshot()?;
    // A partition directory that could not be listed leaves its counts short,
    // so status fails like an invalid cursor store rather than reporting them.
    let unreadable_partitions = report
        .partitions
        .iter()
        .any(|row| !row.unreadable.is_empty());
    let status = i32::from(
        report.cursors.state == "invalid"
            || report.stream.state == "invalid"
            || unreadable_partitions,
    );
    if parsed.flag("json") {
        write_json(&report)?;
        return Ok(status);
    }
    let mut out = std::io::stdout().lock();
    writeln!(out, "data dir: {}", report.data_dir.display())?;
    if let Some(source) = &report.selected_source {
        writeln!(
            out,
            "selected source: {} ({} at {})",
            source.id,
            source.runtime,
            source.root.display()
        )?;
    } else {
        writeln!(
            out,
            "selected source: none (stream discovery remains automatic)"
        )?;
    }
    if report.partitions.is_empty() {
        writeln!(out, "partitions: none (the stream has not recorded events)")?;
    }
    for row in &report.partitions {
        writeln!(
            out,
            "  {}: {} partition files, {} bytes",
            row.runtime, row.parts, row.bytes
        )?;
        for unreadable in &row.unreadable {
            writeln!(out, "    not counted, cannot be read: {unreadable}")?;
        }
    }
    let newest = match &report.cursors.newest_source_mtime {
        Some(stamp) => format!(", newest {stamp}"),
        None => String::new(),
    };
    let tracked = match report.cursors.files {
        Some(files) => format!(", {files} tracked files"),
        None => String::new(),
    };
    writeln!(out, "cursors: {}{tracked}{newest}", report.cursors.state)?;
    if let Some(error) = &report.cursors.error {
        writeln!(out, "  cursor error: {error}")?;
    }
    let summary = report
        .stream
        .summary
        .as_ref()
        .filter(|value| !value.is_null());
    match summary {
        Some(current) => {
            let state = js_string(current.get("state"));
            let updated = current
                .get("updatedAt")
                .or_else(|| current.get("startedAt"));
            // A count the stream state does not carry is said to be missing,
            // not printed as none.
            let count = |field: &str| match current.get(field).and_then(Value::as_u64) {
                Some(count) => count.to_string(),
                None => "not reported".to_string(),
            };
            writeln!(
                out,
                "stream: {state}, updated {}, files {}, failures {}",
                js_string(updated),
                count("filesStreamed"),
                count("failures")
            )?
        }
        None => writeln!(out, "stream: {}", report.stream.state)?,
    }
    if let Some(error) = &report.stream.error {
        writeln!(out, "  stream state error: {error}")?;
    }
    match &report.oko {
        Value::String(text) => writeln!(out, "oko: {text}")?,
        other => writeln!(out, "oko: {}", serde_json::to_string(other)?)?,
    }
    Ok(status)
}
