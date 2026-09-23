//! Running the stream as a service: what one structured line looks like, how
//! a stop signal is observed, and where the live state is written so another
//! command can read what the stream is doing without asking this process.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, RecvTimeoutError};
use std::sync::atomic::{AtomicBool, Ordering};

use notify::{RecursiveMode, Watcher};
use serde_json::{json, Map, Value};

use crate::args::{parse_options, require_flags_only};
use crate::paths::{hook_source_roots, STREAM_STATUS_FILE};
use crate::util::{now_iso, Error, Result};

use super::{source_roots, TICK};

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

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_stop_signal(_signal: i32) {
    STOP.store(true, Ordering::SeqCst);
}

/// Ask for a clean stop on SIGINT and SIGTERM instead of the default kill, so
/// the loop returns the success status a supervisor expects from a requested
/// shutdown.
pub(super) fn install_stop_handlers() {
    extern "C" {
        fn signal(signal: i32, handler: usize) -> usize;
    }
    const SIGINT: i32 = 2;
    const SIGTERM: i32 = 15;
    let handler = on_stop_signal as extern "C" fn(i32) as usize;
    unsafe {
        signal(SIGINT, handler);
        signal(SIGTERM, handler);
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

fn process_paths(json_output: bool, data_dir: &Path, paths: &[PathBuf]) {
    match crate::stream::stream_paths(data_dir, paths) {
        Ok(summary) => {
            let files = summary
                .get("filesStreamed")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let failures = summary.get("failures").and_then(Value::as_u64).unwrap_or(0);
            let duration = summary.get("durationMs").cloned().unwrap_or(Value::Null);
            log(
                json_output,
                "commit",
                &[
                    ("files", Value::from(files)),
                    ("failures", Value::from(failures)),
                    ("ms", duration.clone()),
                ],
            );
            let state = json!({
                "state": "running",
                "updatedAt": now_iso(),
                "paths": paths.len(),
                "filesStreamed": files,
                "failures": failures,
                "durationMs": duration,
            });
            if let Err(error) = write_stream_state(data_dir, &state) {
                log(
                    json_output,
                    "state-error",
                    &[("error", Value::String(error.to_string()))],
                );
            }
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
            let _ = write_stream_state(data_dir, &state);
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
/// are deduplicated without a timer or quiet-period delay.
pub fn stream(rest: &[String]) -> Result<i32> {
    let parsed = parse_options("stream", rest, &[], &["json"])?;
    require_flags_only("stream", &parsed)?;
    let json_output = parsed.flag("json");
    super::predecessor::retire();
    let data_dir = resolve_data_dir(None);
    remove_obsolete_summary(&data_dir)?;
    let roots = source_roots(&data_dir)?;
    if roots.is_empty() {
        return Err(Error(
            "stream found no supported source roots on this machine".into(),
        ));
    }
    let (sender, receiver) = channel();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        if let Ok(event) = event {
            for path in event.paths {
                let _ = sender.send(path);
            }
        }
    })
    .map_err(|error| Error(format!("stream could not start: {error}")))?;
    for root in &roots {
        watcher
            .watch(root, RecursiveMode::Recursive)
            .map_err(|error| {
                Error(format!(
                    "stream could not watch {}: {error}",
                    root.display()
                ))
            })?;
    }
    install_stop_handlers();
    let started_at = now_iso();
    write_stream_state(
        &data_dir,
        &json!({"state": "catching-up", "startedAt": started_at, "roots": roots.len()}),
    )?;
    log(json_output, "start", &[("roots", Value::from(roots.len()))]);

    // Watch first, then close any cursor gap left by downtime. Notifications
    // arriving during this pass stay queued and become cheap cursor no-ops.
    let summary = crate::stream::catch_up(&data_dir)?;
    let discovered = summary
        .get("filesDiscovered")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let streamed = summary
        .get("filesStreamed")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let failures = summary.get("failures").and_then(Value::as_u64).unwrap_or(0);
    let duration = summary.get("durationMs").cloned().unwrap_or(Value::Null);
    log(
        json_output,
        "catch-up",
        &[
            ("files", Value::from(discovered)),
            ("streamed", Value::from(streamed)),
            ("failures", Value::from(failures)),
            ("ms", duration.clone()),
        ],
    );
    write_stream_state(
        &data_dir,
        &json!({
            "state": if failures == 0 { "running" } else { "degraded" },
            "startedAt": started_at,
            "updatedAt": now_iso(),
            "roots": roots.len(),
            "filesDiscovered": discovered,
            "filesStreamed": streamed,
            "failures": failures,
            "durationMs": duration,
        }),
    )?;

    while !STOP.load(Ordering::SeqCst) {
        let first = match receiver.recv_timeout(TICK) {
            Ok(path) => path,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        };
        let mut pending = BTreeSet::new();
        pending.insert(first);
        pending.extend(receiver.try_iter());
        let paths: Vec<PathBuf> = pending.into_iter().collect();
        log(
            json_output,
            "event",
            &[
                ("paths", Value::from(paths.len())),
                ("first", Value::String(paths[0].display().to_string())),
            ],
        );
        process_paths(json_output, &data_dir, &paths);
    }
    for root in &roots {
        let _ = watcher.unwatch(root);
    }
    write_stream_state(
        &data_dir,
        &json!({"state": "stopped", "updatedAt": now_iso(), "roots": roots.len()}),
    )?;
    log(json_output, "stop", &[]);
    Ok(0)
}
