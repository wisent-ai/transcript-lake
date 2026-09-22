//! Validate, ingest, and select one discovered transcript source.
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::args::{parse_options, require_flags_only, require_runtime};
use crate::paths::resolve_data_dir;
use crate::types::{Adapter, ParserCtx};
use crate::util::{home_dir, write_json, Error, Result};

#[derive(Debug)]
pub(super) struct Preflight {
    pub(super) files: u64,
    pub(super) events: u64,
    pub(super) pending_lines: u64,
}

pub fn adopt(rest: &[String]) -> Result<i32> {
    let parsed = parse_options("adopt", rest, &["source", "root"], &["json"])?;
    require_flags_only("adopt", &parsed)?;
    let Some(runtime) = require_runtime(parsed.value("source"))? else {
        return Err(Error("adopt requires --source <runtime>".into()));
    };
    let Some(root) = parsed.value("root") else {
        return Err(Error("adopt requires --root <discovered-root>".into()));
    };
    let report = adopt_source(&runtime, Path::new(root), &resolve_data_dir(None))?;
    if parsed.flag("json") {
        write_json(&report)?;
        return Ok(0);
    }
    let mut out = std::io::stdout().lock();
    writeln!(
        out,
        "source {}: {}",
        report.get("sourceId").and_then(Value::as_str).unwrap_or_default(),
        report.get("status").and_then(Value::as_str).unwrap_or("adopted")
    )?;
    writeln!(
        out,
        "runtime: {}",
        report.get("runtime").and_then(Value::as_str).unwrap_or_default()
    )?;
    writeln!(
        out,
        "root: {}",
        report.get("root").and_then(Value::as_str).unwrap_or_default()
    )?;
    writeln!(
        out,
        "files: {} imported, {} unchanged; events: {} imported",
        report.get("imported").and_then(Value::as_u64).unwrap_or(0),
        report.get("unchanged").and_then(Value::as_u64).unwrap_or(0),
        report
            .get("eventsImported")
            .and_then(Value::as_u64)
            .unwrap_or(0)
    )?;
    Ok(0)
}

/// The reusable adoption operation used by the CLI and first-use journey.
///
/// Every candidate is opened and every complete nonempty line must be JSON
/// before the writer lease is acquired. Only then does the canonical stream
/// mask, partition, project to Oko, and advance byte cursors. The selection is
/// persisted after that boundary returns successfully.
pub fn adopt_source(runtime: &str, requested_root: &Path, data_dir: &Path) -> Result<Value> {
    let adapter = crate::adapters::by_name(runtime).ok_or_else(|| {
        Error(format!(
            "source \"{runtime}\" is not an adoptable transcript runtime (expected one of: claude, codex, omp, droid, kimi)"
        ))
    })?;
    let root = canonical_supported_root(adapter.as_ref(), requested_root)?;
    let preflight = preflight(adapter.as_ref(), &root)?;
    let ingest = crate::stream::ingest_source(data_dir, runtime, &root)?;
    let imported = ingest
        .get("filesImported")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let unchanged = ingest
        .get("filesUnchanged")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let events_imported = ingest
        .get("eventsImported")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    if imported + unchanged != preflight.files {
        return Err(Error(format!(
            "source ingestion accepted {} of {} preflighted transcript files; selection was not changed",
            imported + unchanged,
            preflight.files
        )));
    }
    if events_imported == 0 && unchanged == 0 {
        return Err(Error(
            "source ingestion persisted no canonical events; selection was not changed".into(),
        ));
    }
    let selected = crate::sources::persist_selection(data_dir, runtime, &root)?;
    Ok(json!({
        "status": if imported == 0 { "unchanged" } else { "adopted" },
        "sourceId": selected.id,
        "runtime": selected.runtime,
        "root": selected.root,
        "selected": true,
        "candidateFiles": preflight.files,
        "candidateEvents": preflight.events,
        "pendingIncompleteLines": preflight.pending_lines,
        "imported": imported,
        "unchanged": unchanged,
        "conflicting": 0,
        "rejected": 0,
        "eventsImported": events_imported,
        "dataDir": data_dir,
    }))
}

mod preflight;

use preflight::{canonical_supported_root, preflight};
