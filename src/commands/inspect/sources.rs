//! Which sources this machine can read, and which one this Lake follows:
//! every transcript adapter in stream order, then the hooks, each with the
//! candidate files it can see and the reason it cannot, when it cannot.

use std::fs;
use std::io::Write;
use std::path::PathBuf;

use crate::args::{parse_options, require_flags_only};
use crate::paths::hook_source_roots;
use crate::types::HOOKS;
use crate::util::{home_dir, write_json, Result};

use super::SourceRow;


pub(super) fn display_roots(roots: &[PathBuf]) -> Vec<String> {
    roots
        .iter()
        .map(|root| root.to_string_lossy().into_owned())
        .collect()
}

/// Adaptive-hook telemetry is not a transcript adapter: it arrives either as
/// closed segments under the ready directory or as the legacy mutable log, and
/// the file count means a different thing in each mode.
pub(super) fn hooks_row() -> SourceRow {
    let hooks = hook_source_roots();
    let files = if hooks.segment_mode {
        match fs::read_dir(&hooks.ready) {
            Ok(entries) => entries
                .flatten()
                .filter(|entry| {
                    entry
                        .file_type()
                        .map(|kind| kind.is_file())
                        .unwrap_or(false)
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
                    source_ids: Vec::new(),
                    selected: false,
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
        source_ids: Vec::new(),
        selected: false,
        files,
        error: None,
    }
}

/// Availability and candidate-file counts for every supported source, in the
/// order used by the stream: every transcript adapter, then hooks.
pub fn source_report() -> Result<Vec<SourceRow>> {
    let home = home_dir();
    let data_dir = crate::paths::resolve_data_dir(None);
    let selected = crate::sources::selected_source(&data_dir)?;
    let mut rows = Vec::new();
    for adapter in crate::adapters::all() {
        let roots = adapter.roots(&home);
        let files = roots
            .iter()
            .map(|root| adapter.list_sessions(root).len() as u64)
            .sum();
        let canonical_roots: Vec<PathBuf> = roots
            .iter()
            .map(|root| fs::canonicalize(root).unwrap_or_else(|_| root.clone()))
            .collect();
        rows.push(SourceRow {
            runtime: adapter.runtime().to_string(),
            available: !roots.is_empty(),
            mode: "transcripts",
            roots: display_roots(&roots),
            source_ids: canonical_roots
                .iter()
                .map(|root| crate::sources::source_identity(adapter.runtime(), root))
                .collect(),
            selected: selected.as_ref().is_some_and(|source| {
                source.runtime == adapter.runtime() && canonical_roots.contains(&source.root)
            }),
            files,
            error: None,
        });
    }
    rows.push(hooks_row());
    Ok(rows)
}

pub fn sources(rest: &[String]) -> Result<i32> {
    let parsed = parse_options("sources", rest, &[], &["json"])?;
    require_flags_only("sources", &parsed)?;
    let rows = source_report()?;
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
        writeln!(
            out,
            "{}: {state}, {} files{}{}",
            row.runtime,
            row.files,
            if row.selected { ", selected" } else { "" },
            suffix
        )?;
        for (index, root) in row.roots.iter().enumerate() {
            let identity = row.source_ids.get(index).map(String::as_str).unwrap_or("");
            if identity.is_empty() {
                writeln!(out, "  {root}")?;
            } else {
                writeln!(out, "  {root}  [{identity}]")?;
            }
        }
    }
    Ok(status)
}
