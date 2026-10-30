//! Running the stream as a service: what one structured line looks like and
//! where the live state is written so another command can read what the
//! stream is doing without asking this process.
//!
//! A catch-up that cannot run (a missing masking declaration, an unreadable
//! cursor store) is kept as the state `failed` with its error before the
//! process exits with it, and the next start carries that failure as
//! `lastFailure` until a catch-up completes: a stream its supervisor restarts
//! after every failure reads as failing, never as catching up.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::channel;

use serde_json::{json, Map, Value};

use crate::args::{parse_options, require_flags_only};
use crate::paths::{resolve_data_dir, STREAM_STATUS_FILE};
use crate::util::{now_iso, Error, Result};

use super::source_roots;
use super::watch::{unwatch, watch, Wake};

/// One structured stream line: JSON when requested, otherwise timestamped
/// key=value text for service logs.
pub(super) fn log(json: bool, kind: &str, details: &[(&str, Value)]) {
    let ts = now_iso();
    if json {
        let mut record = Map::new();
        record.insert("ts".to_string(), Value::String(ts));
        record.insert("kind".to_string(), Value::String(kind.to_string()));
        for (key, value) in details {
            record.insert((*key).to_string(), value.clone());
        }
        println!("{}", Value::Object(record));
        return;
    }
    let text = details
        .iter()
        .map(|(key, value)| {
            let rendered = match value {
                Value::String(text) => text.clone(),
                other => other.to_string(),
            };
            format!("{key}={rendered}")
        })
        .collect::<Vec<String>>()
        .join(" ");
    if text.is_empty() {
        println!("{ts} stream {kind}");
    } else {
        println!("{ts} stream {kind} {text}");
    }
}

pub(super) fn write_stream_state(data_dir: &Path, state: &Value) -> Result<()> {
    fs::create_dir_all(data_dir)?;
    let path = data_dir.join(STREAM_STATUS_FILE);
    let temporary = data_dir.join(format!("{STREAM_STATUS_FILE}.tmp-{}", std::process::id()));
    fs::write(
        &temporary,
        format!("{}\n", serde_json::to_string_pretty(state)?),
    )?;
    fs::rename(temporary, path)?;
    Ok(())
}

/// Write `state`, or say on the log why it could not be kept.
fn keep_state(json_output: bool, data_dir: &Path, state: &Value) {
    if let Err(error) = write_stream_state(data_dir, state) {
        log(
            json_output,
            "state-error",
            &[("error", Value::String(error.to_string()))],
        );
    }
}

/// The failure the previous start kept, carried while this start has not
/// caught up: its own `failed` state, or the one it still carried. A state
/// file that is absent carries none; one that cannot be read is itself the
/// failure carried.
fn previous_failure(data_dir: &Path) -> Value {
    let path = data_dir.join(STREAM_STATUS_FILE);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Value::Null,
        Err(error) => {
            let cause = format!("{} cannot be read: {error}", path.display());
            return json!({ "error": cause });
        }
    };
    match serde_json::from_str::<Value>(&text) {
        Ok(state) if state["state"] == "failed" => {
            json!({ "at": state["failedAt"], "error": state["error"], "starts": state["failedStarts"] })
        }
        Ok(state) => state["lastFailure"].clone(),
        Err(error) => json!({ "error": format!("{} is not JSON: {error}", path.display()) }),
    }
}

/// A pass's state, from its summary: `degraded` when a file failed.
fn passed(summary: &Value) -> &'static str {
    match summary["partial"] == true {
        true => "degraded",
        false => "running",
    }
}

fn process_paths(json_output: bool, data_dir: &Path, paths: &[PathBuf]) {
    match crate::stream::stream_paths(data_dir, paths) {
        Ok(summary) => {
            log(
                json_output,
                "commit",
                &[
                    ("files", summary["filesStreamed"].clone()),
                    ("failures", summary["failures"].clone()),
                    ("ms", summary["durationMs"].clone()),
                ],
            );
            let state = json!({
                "state": passed(&summary),
                "updatedAt": now_iso(),
                "paths": paths.len(),
                "filesStreamed": summary["filesStreamed"],
                "failures": summary["failures"],
                "durationMs": summary["durationMs"],
            });
            keep_state(json_output, data_dir, &state);
        }
        Err(error) => {
            log(
                json_output,
                "error",
                &[("error", Value::String(error.to_string()))],
            );
            let state = json!({
                "state": "degraded",
                "updatedAt": now_iso(),
                "paths": paths.len(),
                "error": error.to_string(),
            });
            keep_state(json_output, data_dir, &state);
        }
    }
}

fn remove_obsolete_summary(data_dir: &Path) -> Result<()> {
    let path = data_dir.join("last-ingest.json");
    match fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(Error(format!(
            "could not remove obsolete {}: {error}",
            path.display()
        ))),
    }
}

/// Follow source writes continuously. Each notification is consumed
/// immediately; notifications already queued by the same filesystem operation
/// are deduplicated without a timer or quiet-period delay. Every error the
/// run returns once it knows its data directory (no source root, a root it
/// cannot watch, the stop handler, the catch-up) is kept as `failed` with the
/// error before the process exits with it.
pub fn stream(rest: &[String]) -> Result<i32> {
    let parsed = parse_options("stream", rest, &[], &["json"])?;
    require_flags_only("stream", &parsed)?;
    let json_output = parsed.flag("json");
    super::predecessor::retire();
    let data_dir = resolve_data_dir(None);
    let started_at = now_iso();
    following(json_output, &data_dir, &started_at).inspect_err(|error| {
        let failed = json!({
            "state": "failed",
            "startedAt": started_at,
            "failedAt": now_iso(),
            "error": error.to_string(),
            "failedStarts": crate::lake::paths::failed_starts(&previous_failure(&data_dir)),
        });
        keep_state(json_output, &data_dir, &failed);
    })
}

/// One run of the stream from its start to a requested stop.
fn following(json_output: bool, data_dir: &Path, started_at: &str) -> Result<i32> {
    let data_dir = data_dir.to_path_buf();
    remove_obsolete_summary(&data_dir)?;
    let roots = source_roots(&data_dir)?;
    if roots.is_empty() {
        return Err(Error(
            "stream found no supported source roots on this machine".into(),
        ));
    }
    let (sender, receiver) = channel();
    let mut watcher = watch(&roots, sender.clone())?;
    super::signals::on_stop(sender, Wake::Stop)?;
    // A start after a failed one carries that failure until a catch-up
    // completes; its process is recorded so `status` tells it from one whose
    // process ended without finishing.
    let last_failure = previous_failure(&data_dir);
    let process = crate::lake::owner::own_record("the stream's catch-up record")?;
    write_stream_state(
        &data_dir,
        &json!({
            "state": "catching-up", "startedAt": started_at, "roots": roots.len(),
            "lastFailure": last_failure, "process": process,
        }),
    )?;
    log(json_output, "start", &[("roots", Value::from(roots.len()))]);

    // Watch first, then close any cursor gap left by downtime. Notifications
    // arriving during this pass stay queued and become cheap cursor no-ops.
    let summary = crate::stream::catch_up(&data_dir)?;
    log(
        json_output,
        "catch-up",
        &[
            ("files", summary["filesDiscovered"].clone()),
            ("streamed", summary["filesStreamed"].clone()),
            ("failures", summary["failures"].clone()),
            ("ms", summary["durationMs"].clone()),
        ],
    );
    write_stream_state(
        &data_dir,
        &json!({
            "state": passed(&summary),
            "startedAt": started_at,
            "updatedAt": now_iso(),
            "roots": roots.len(),
            "filesDiscovered": summary["filesDiscovered"],
            "filesStreamed": summary["filesStreamed"],
            "failures": summary["failures"],
            "durationMs": summary["durationMs"],
        }),
    )?;

    // Blocks until a path changes, the watcher fails or a stop signal
    // arrives; no tick.
    while let Ok(first) = receiver.recv() {
        let mut pending = BTreeSet::new();
        let mut failures = Vec::new();
        let mut stop = false;
        for wake in std::iter::once(first).chain(receiver.try_iter()) {
            match wake {
                Wake::Path(path) => {
                    pending.insert(path);
                }
                Wake::Failed(error) => failures.push(error),
                Wake::Stop => stop = true,
            }
        }
        if !failures.is_empty() {
            // The watcher lost changes it cannot name, so every root is
            // caught up again from its cursors.
            log(json_output, "watch-failed", &[("errors", json!(failures))]);
            let summary = crate::stream::catch_up(&data_dir)?;
            log(
                json_output,
                "catch-up",
                &[
                    ("files", summary["filesDiscovered"].clone()),
                    ("streamed", summary["filesStreamed"].clone()),
                    ("failures", summary["failures"].clone()),
                    ("ms", summary["durationMs"].clone()),
                ],
            );
        }
        if let Some(first) = pending.first().map(|path| path.display().to_string()) {
            let paths: Vec<PathBuf> = pending.into_iter().collect();
            log(
                json_output,
                "event",
                &[
                    ("paths", Value::from(paths.len())),
                    ("first", Value::String(first)),
                ],
            );
            process_paths(json_output, &data_dir, &paths);
        }
        if stop {
            break;
        }
    }
    let unwatch_failures = unwatch(&mut watcher, &roots);
    let stopped = json!({
        "state": "stopped", "updatedAt": now_iso(), "roots": roots.len(),
        "unwatchFailures": unwatch_failures,
    });
    write_stream_state(&data_dir, &stopped)?;
    log(json_output, "stop", &[]);
    Ok(Default::default())
}
