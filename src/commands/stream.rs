//! Recovery replay and the real-time source stream.
use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, RecvTimeoutError};
use std::time::Duration;

use notify::{RecursiveMode, Watcher};
use serde_json::{json, Map, Value};

use crate::args::{parse_options, require_flags_only, require_runtime};
use crate::paths::{hook_source_roots, resolve_data_dir, STREAM_STATUS_FILE};
use crate::stream::{replay as run_replay, ReplayOptions};
use crate::util::{absolute, home_dir, now_iso, write_json, Error, Result};

/// How often the foreground loop wakes to observe a stop signal.
const TICK: Duration = Duration::from_millis(250);

fn perform_replay(source: Option<String>, data_dir: PathBuf) -> Result<i32> {
    let summary = run_replay(ReplayOptions { source, data_dir })?;
    let partial = summary
        .get("partial")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    write_json(&summary)?;
    Ok(i32::from(partial))
}

/// Lexical resolution, matching `path.resolve`: absolute, with `.` and `..`
/// removed, so the comparison against the current Lake cannot be defeated by
/// spelling the same directory differently.
fn resolve_target(value: &str) -> PathBuf {
    let mut out = PathBuf::new();
    for component in absolute(value).components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

pub fn rebuild(rest: &[String]) -> Result<i32> {
    let parsed = parse_options("rebuild", rest, &["to", "source"], &[])?;
    require_flags_only("rebuild", &parsed)?;
    let Some(to) = parsed.value("to") else {
        return Err(Error("rebuild requires --to <empty-path>".into()));
    };
    let current = resolve_data_dir(None);
    let target = resolve_target(to);
    if target == current {
        return Err(Error(
            "rebuild target must differ from the current Lake".into(),
        ));
    }
    // A full replay is only ever allowed into the target root: the current
    // Lake is neither read for state nor written to.
    perform_replay(require_runtime(parsed.value("source"))?, target)
}

/// Stable directories watched recursively for source appends. Runtime-specific
/// workspace directories may appear after startup, so the stream watches their
/// persistent parents rather than a one-time enumeration of current children.
fn source_roots() -> Vec<PathBuf> {
    let home = home_dir();
    let mut roots = vec![
        home.join(".claude").join("projects"),
        home.join(".codex").join("sessions"),
        home.join(".omp").join("agent").join("sessions"),
        home.join(".factory").join("sessions"),
        home.join(".kimi-code").join("sessions"),
    ];
    roots.extend(hook_source_roots().roots);
    roots.retain(|root| root.exists());
    roots.sort();
    roots.dedup();
    roots
}

/// One structured stream line: JSON when requested, otherwise timestamped
/// key=value text for service logs.
fn log(json: bool, kind: &str, details: &[(&str, Value)]) {
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
fn install_stop_handlers() {
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

fn write_stream_state(data_dir: &Path, state: &Value) -> Result<()> {
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
    let data_dir = resolve_data_dir(None);
    remove_obsolete_summary(&data_dir)?;
    let roots = source_roots();
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
        &json!({"state": "running", "startedAt": started_at, "roots": roots.len()}),
    )?;
    log(json_output, "start", &[("roots", Value::from(roots.len()))]);

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
