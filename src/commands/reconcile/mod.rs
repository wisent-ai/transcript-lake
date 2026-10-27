//! `reconcile --source <runtime> [--apply] [--reindex]`: replay every
//! retained original of one runtime through the current adapter into a
//! staging root, compare each canonical source partition with what the
//! replay wrote, and with `--apply` replace each partition whose recorded
//! projection no longer follows from its original. Corrected rows then reach
//! Oko through a full projection rebuild (`--reindex` hands it to Oko).
//! A partition no original on this machine reproduces, or one holding other
//! machines' rows, is reported and never rewritten.

mod partitions;

use std::fs;
use std::io;
use std::path::Path;
use std::process::Command;

use serde_json::{json, Value};

use crate::args::{parse_options, require_flags_only, require_runtime};
use crate::cursors::{open_writer_lease, CursorRecord, Cursors};
use crate::oko_export::{export_oko_with_reindex, remove_tree};
use crate::paths::{oko_cli, resolve_data_dir};
use crate::stream::{replay, ReplayOptions};
use crate::types::HOOKS;
use crate::util::{home_dir, machine_name, write_json, Error, Result};

use partitions::{compare, Partition, Verdict};

struct Outcome {
    report: Value,
    /// Whether any partition was rewritten.
    rewritten: bool,
    /// Whether every recorded partition was proved against its original.
    proved: bool,
}

fn sessions(partition: &Partition) -> Value {
    json!(partition.sessions)
}

fn describe(partitions: &[Partition]) -> Value {
    let mut changed = Vec::new();
    let mut unprovable = Vec::new();
    let mut refused = Vec::new();
    for partition in partitions {
        match &partition.verdict {
            Verdict::Unchanged => {}
            Verdict::Changed { removed, added } => changed.push(json!({
                "partition": partition.name,
                "sessions": sessions(partition),
                "rowsRecorded": partition.rows,
                "rowsSuperseded": removed,
                "rowsReplayed": added,
            })),
            Verdict::Unprovable => unprovable.push(json!({
                "partition": partition.name,
                "sessions": sessions(partition),
                "rowsRecorded": partition.rows,
                "reason": "no retained original on this machine reproduces this partition; its rows are kept as recorded and are not reclassified",
            })),
            Verdict::Refused { machines } => refused.push(json!({
                "partition": partition.name,
                "sessions": sessions(partition),
                "rowsRecorded": partition.rows,
                "machines": machines,
                "reason": "the partition holds rows this machine's original cannot stand for",
            })),
        }
    }
    let unchanged = partitions
        .iter()
        .filter(|partition| matches!(partition.verdict, Verdict::Unchanged))
        .count();
    json!({
        "unchanged": unchanged,
        "changed": changed,
        "unprovable": unprovable,
        "refused": refused,
    })
}

/// Give the current Lake the replay's cursor for every source of the
/// runtime, so the live stream resumes after what the replaced partitions
/// already hold instead of appending it again.
fn adopt_cursors(data_dir: &Path, staging: &Path, runtime: &str) -> Result<()> {
    let adapter = crate::adapters::by_name(runtime)
        .ok_or_else(|| Error(format!("transcript adapter unavailable: {runtime}")))?;
    let replayed = Cursors::open(staging)?;
    let mut current = Cursors::open(data_dir)?;
    for root in adapter.roots(&home_dir()) {
        for entry in adapter.list_sessions(&root) {
            let key = entry.file.to_string_lossy().to_string();
            if let Some(CursorRecord::Bytes(cursor)) = replayed.get(&key)? {
                current.set_bytes(&key, cursor);
            }
        }
    }
    current.flush()
}

fn remove_mirror(data_dir: &Path, runtime: &str) -> Result<Value> {
    let mirror = data_dir
        .join("parquet")
        .join(format!("runtime={runtime}"))
        .join("events.parquet");
    match fs::remove_file(&mirror) {
        Ok(()) => Ok(json!(mirror.to_string_lossy())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Value::Null),
        Err(error) => Err(Error(format!(
            "cannot remove the superseded Parquet mirror {}: {error}",
            mirror.display()
        ))),
    }
}

fn reconcile_locked(
    data_dir: &Path,
    staging: &Path,
    runtime: &str,
    apply: bool,
) -> Result<Outcome> {
    let replayed = replay(ReplayOptions {
        source: Some(runtime.to_string()),
        data_dir: staging.to_path_buf(),
    })?;
    let partial = match replayed.get("partial") {
        Some(Value::Bool(partial)) => *partial,
        other => {
            return Err(Error(format!(
                "the staging replay states no partial flag: {other:?}"
            )))
        }
    };
    let partitions = compare(data_dir, staging, runtime, &machine_name())?;
    let proved = !partial
        && partitions.iter().all(|partition| {
            matches!(
                partition.verdict,
                Verdict::Unchanged | Verdict::Changed { .. }
            )
        });
    let refused = partitions
        .iter()
        .any(|partition| matches!(partition.verdict, Verdict::Refused { .. }));
    let changed: Vec<&Partition> = partitions
        .iter()
        .filter(|partition| matches!(partition.verdict, Verdict::Changed { .. }))
        .collect();
    let mut report = json!({
        "runtime": runtime,
        "replay": replayed,
        "partitions": describe(&partitions),
    });
    let refusal = if partial {
        Some("the replay could not read every original (named on stderr), so its partitions cannot stand for them")
    } else if refused {
        Some("a partition holds rows this machine's original cannot stand for")
    } else {
        None
    };
    let rewritten = apply && refusal.is_none() && !changed.is_empty();
    if rewritten {
        for partition in &changed {
            partition.apply(data_dir, runtime)?;
        }
        adopt_cursors(data_dir, staging, runtime)?;
        report["parquetMirrorRemoved"] = remove_mirror(data_dir, runtime)?;
    }
    report["applied"] = json!(rewritten);
    if apply {
        if let Some(refusal) = refusal {
            report["applyRefused"] = json!(refusal);
        }
    }
    Ok(Outcome {
        report,
        rewritten,
        proved,
    })
}

/// Hand the rebuilt projection to Oko through its own CLI. Its output goes
/// to stderr so this command's stdout stays one JSON document.
fn reindex_oko() -> Result<()> {
    let binary = oko_cli()
        .ok_or_else(|| {
            Error("oko-cli is not on PATH and OKO_CLI names none; the corrected projection is written, and `transcript-lake projection refresh --target oko` hands it to Oko".into())
        })?;
    // The reindex runs as long as Oko takes; stado_wait says on stderr what
    // it waits on, where and since when, so a long one names itself.
    let status = stado_wait::status(
        Command::new(&binary)
            .args(["transcripts", "reindex"])
            .stdout(io::stderr()),
    )
    .map_err(|error| {
        Error(format!(
            "cannot run {} transcripts reindex: {error}",
            binary.display()
        ))
    })?;
    if status.success() {
        return Ok(());
    }
    Err(Error(format!(
        "{} transcripts reindex ended with {status}; the corrected projection is written under exports/oko",
        binary.display()
    )))
}

pub fn reconcile(rest: &[String]) -> Result<i32> {
    let parsed = parse_options("reconcile", rest, &["source"], &["apply", "reindex"])?;
    require_flags_only("reconcile", &parsed)?;
    let Some(runtime) = require_runtime(parsed.value("source"))? else {
        return Err(Error("reconcile requires --source <runtime>".into()));
    };
    if runtime == HOOKS {
        return Err(Error(format!(
            "reconcile replays transcript originals; {HOOKS} segments are immutable and content-addressed, so no projection of them can be superseded"
        )));
    }
    let apply = parsed.flag("apply");
    let reindex = parsed.flag("reindex");
    if reindex && !apply {
        return Err(Error(
            "reconcile --reindex needs --apply: only an applied correction changes what Oko imports".into(),
        ));
    }
    let data_dir = resolve_data_dir(None);
    let staging = data_dir
        .join("staging")
        .join(format!("reconcile-{runtime}"));
    remove_tree(&staging)?;
    let mut lease = open_writer_lease(&data_dir)?;
    let outcome = reconcile_locked(&data_dir, &staging, &runtime, apply);
    lease.close();
    let removal = remove_tree(&staging).map_err(|left| {
        format!(
            "reconcile staging {} was left behind: {left}",
            staging.display()
        )
    });
    let outcome = match (outcome, removal) {
        (Ok(outcome), removal) => {
            // Partitions already replaced are corrected in Lake; Oko is
            // given them even when the staging root stays behind.
            let mut outcome = outcome;
            if let Err(left) = &removal {
                outcome.report["stagingLeftBehind"] = json!(left);
            }
            if outcome.rewritten {
                // The projection export takes the writer lease itself.
                outcome.report["projection"] = export_oko_with_reindex(true, false, &data_dir)?;
            }
            if let Err(left) = removal {
                write_json(&outcome.report)?;
                return Err(Error(left));
            }
            outcome
        }
        (Err(error), Ok(())) => return Err(error),
        (Err(error), Err(left)) => return Err(Error(format!("{error}; {left}"))),
    };
    write_json(&outcome.report)?;
    if outcome.rewritten && reindex {
        reindex_oko()?;
    }
    Ok(i32::from(!outcome.proved))
}
