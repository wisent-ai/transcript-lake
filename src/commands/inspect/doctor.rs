//! What `doctor` checks, one named check at a time, and the verdict it prints
//! when a check fails: what is wrong and the command that repairs it.

use std::io::Write;
use std::path::PathBuf;

use serde::Serialize;

use crate::args::{parse_options, require_flags_only};
use crate::paths::{lake_paths, read_cursor_status};

use super::{source_report, SourceRow};
use crate::util::{write_json, Result};

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
    let sources = source_report()?;
    let broken: Vec<&SourceRow> = sources.iter().filter(|row| row.error.is_some()).collect();
    let checks = vec![
        Check {
            name: "state-root",
            // An absent state root is the zero state; the stream creates it
            // when the service starts.
            status: "ok",
            detail: if paths.data_dir.exists() {
                paths.data_dir.to_string_lossy().into_owned()
            } else {
                format!("absent zero-state: {}", paths.data_dir.display())
            },
        },
        Check {
            name: "cursors",
            status: if cursors.state == "invalid" {
                "error"
            } else {
                "ok"
            },
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
                        format!(
                            "{}: {}",
                            row.runtime,
                            row.error.as_deref().unwrap_or_default()
                        )
                    })
                    .collect::<Vec<String>>()
                    .join("; ")
            },
        },
        Check {
            name: "duckdb",
            status: if paths.duckdb.is_some() {
                "ok"
            } else {
                "warning"
            },
            detail: match &paths.duckdb {
                Some(found) => found.to_string_lossy().into_owned(),
                None => {
                    "optional dependency not found; analytics and compact unavailable".to_string()
                }
            },
        },
        Check {
            name: "oko-cli",
            status: if paths.oko_cli.is_some() {
                "ok"
            } else {
                "warning"
            },
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
