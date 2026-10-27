//! Validate, ingest, and select one discovered transcript source.
use std::io::Write;
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::args::{parse_options, require_flags_only, require_runtime};
use crate::sources::AdoptedSource;
use crate::paths::resolve_data_dir;
use crate::util::{write_json, Error, Result};

#[derive(Debug)]
pub(super) struct Preflight {
    pub(super) files: u64,
    pub(super) events: u64,
    pub(super) pending_lines: u64,
}

/// What one adoption did: the source it selected, what preflight counted and
/// what ingestion kept. Every reader takes these fields, so no count is read
/// back out of the JSON receipt and none is ever printed as zero because it
/// was missing.
pub struct Adopted {
    pub selected: AdoptedSource,
    pub preflight_files: u64,
    pub preflight_events: u64,
    pub pending_lines: u64,
    pub imported: u64,
    pub unchanged: u64,
    pub events_imported: u64,
    pub data_dir: PathBuf,
}

impl Adopted {
    /// Whether this adoption imported any file, or found all of them unchanged.
    pub fn status(&self) -> &'static str {
        match NonZeroU64::new(self.imported) {
            Some(_) => "adopted",
            None => "unchanged",
        }
    }

    /// The `--json` receipt.
    pub fn report(&self) -> Value {
        json!({
            "status": self.status(),
            "sourceId": self.selected.id,
            "runtime": self.selected.runtime,
            "root": self.selected.root,
            "selected": true,
            "candidateFiles": self.preflight_files,
            "candidateEvents": self.preflight_events,
            "pendingIncompleteLines": self.pending_lines,
            "imported": self.imported,
            "unchanged": self.unchanged,
            "eventsImported": self.events_imported,
            "dataDir": self.data_dir,
        })
    }
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
    let adopted = adopt_source(&runtime, Path::new(root), &resolve_data_dir(None))?;
    if parsed.flag("json") {
        write_json(&adopted.report())?;
        return Ok(libc::EXIT_SUCCESS);
    }
    let mut out = std::io::stdout().lock();
    writeln!(out, "source {}: {}", adopted.selected.id, adopted.status())?;
    writeln!(out, "runtime: {}", adopted.selected.runtime)?;
    writeln!(out, "root: {}", adopted.selected.root.display())?;
    writeln!(
        out,
        "files: {} imported, {} unchanged; events: {} imported",
        adopted.imported, adopted.unchanged, adopted.events_imported
    )?;
    Ok(libc::EXIT_SUCCESS)
}

/// The count `key` of the ingestion summary. A summary without it is refused
/// naming the key: read as zero, a failed ingestion was compared with
/// preflight as if it had imported nothing.
fn counted(ingest: &Value, key: &str) -> Result<u64> {
    ingest.get(key).and_then(Value::as_u64).ok_or_else(|| {
        Error(format!(
            "source ingestion answered no {key} count ({ingest}); selection was not changed"
        ))
    })
}

/// The reusable adoption operation used by the CLI and first-use journey.
///
/// Every candidate is opened and every complete nonempty line must be JSON
/// before the writer lease is acquired. Only then does the canonical stream
/// mask, partition, project to Oko, and advance byte cursors. The selection is
/// persisted after that boundary returns successfully.
pub fn adopt_source(runtime: &str, requested_root: &Path, data_dir: &Path) -> Result<Adopted> {
    let adapter = crate::adapters::by_name(runtime).ok_or_else(|| {
        let runtimes: Vec<&str> = crate::adapters::all().iter().map(|adapter| adapter.runtime()).collect();
        Error(format!(
            "source \"{runtime}\" is not an adoptable transcript runtime (expected one of: {}; `transcript-lake sources` shows each with its roots on this machine)",
            runtimes.join(", ")
        ))
    })?;
    let root = canonical_supported_root(adapter.as_ref(), requested_root)?;
    let preflight = preflight(adapter.as_ref(), &root)?;
    let ingest = crate::stream::ingest_source(data_dir, runtime, &root)?;
    let imported = counted(&ingest, "filesImported")?;
    let unchanged = counted(&ingest, "filesUnchanged")?;
    let events_imported = counted(&ingest, "eventsImported")?;
    if imported + unchanged != preflight.files {
        return Err(Error(format!(
            "source ingestion accepted {} of {} preflighted transcript files; selection was not changed",
            imported + unchanged,
            preflight.files
        )));
    }
    if NonZeroU64::new(events_imported + unchanged).is_none() {
        return Err(Error(
            "source ingestion persisted no canonical events; selection was not changed".into(),
        ));
    }
    let selected = crate::sources::persist_selection(data_dir, runtime, &root)?;
    Ok(Adopted {
        selected,
        preflight_files: preflight.files,
        preflight_events: preflight.events,
        pending_lines: preflight.pending_lines,
        imported,
        unchanged,
        events_imported,
        data_dir: data_dir.to_path_buf(),
    })
}

mod preflight;

use preflight::{canonical_supported_root, preflight};
