//! Ingest, safe replay, and the online watch loop. Every one of them ends in
//! the same refresh the external timer runs — ingest, then the Oko export —
//! so a Lake is never left with events its derived artifacts do not know.
use std::collections::VecDeque;
use std::path::{Component, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, RecvTimeoutError};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use notify::{RecursiveMode, Watcher};
use serde_json::{Map, Value};

use crate::args::{
    bounded_integer, parse_options, require_flags_only, require_runtime, DEFAULT_DEBOUNCE,
    MAX_LIMIT,
};
use crate::ingest::{ingest as run_ingest, IngestOptions};
use crate::paths::{hook_source_roots, resolve_data_dir, SUMMARY_FILE};
use crate::types::{HOOKS, SUPPORTED_SOURCES};
use crate::util::{absolute, home_dir, now_iso, write_json, Error, Result};

const SECOND_MS: u64 = 1000;
/// How often the watch loop wakes to notice a signal while it is otherwise
/// idle.
const TICK: Duration = Duration::from_millis(250);

/// Ingest, export for Oko, then publish the run summary. The summary file is
/// what `status` reads and what the timer's logs quote, so it is written even
/// when the run was partial.
fn perform_ingest(source: Option<String>, full: bool, data_dir: PathBuf) -> Result<i32> {
    let summary = run_ingest(IngestOptions {
        source: source.clone(),
        full,
        data_dir: data_dir.clone(),
    })?;
    let oko_export = crate::oko_export::export_oko(full, &data_dir)?;
    let mut record = Map::new();
    record.insert("finishedAt".to_string(), Value::String(now_iso()));
    record.insert(
        "dataDir".to_string(),
        Value::String(data_dir.to_string_lossy().to_string()),
    );
    record.insert(
        "source".to_string(),
        source.map(Value::String).unwrap_or(Value::Null),
    );
    record.insert("full".to_string(), Value::Bool(full));
    record.insert("okoExport".to_string(), oko_export);
    let partial = summary.get("partial").and_then(Value::as_bool).unwrap_or(false);
    if let Value::Object(fields) = summary {
        for (key, value) in fields {
            record.insert(key, value);
        }
    }
    let record = Value::Object(record);
    std::fs::create_dir_all(&data_dir)?;
    std::fs::write(
        data_dir.join(SUMMARY_FILE),
        format!("{}\n", serde_json::to_string_pretty(&record)?),
    )?;
    write_json(&record)?;
    Ok(if partial { 1 } else { 0 })
}

pub fn ingest(rest: &[String]) -> Result<i32> {
    let parsed = parse_options("ingest", rest, &["source"], &["full"])?;
    require_flags_only("ingest", &parsed)?;
    perform_ingest(
        require_runtime(parsed.value("source"))?,
        parsed.flag("full"),
        resolve_data_dir(None),
    )
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
        return Err(Error("rebuild target must differ from the current Lake".into()));
    }
    // A full replay is only ever allowed into the target root: the current
    // Lake is neither read for state nor written to.
    perform_ingest(require_runtime(parsed.value("source"))?, true, target)
}

/// Watch roots come from the same adapter and hooks discovery that ingest and
/// sources use, so a new runtime store is watched the moment it is supported.
fn watch_roots() -> Vec<PathBuf> {
    let home = home_dir();
    let mut roots = Vec::new();
    for runtime in SUPPORTED_SOURCES {
        if runtime == HOOKS {
            roots.extend(hook_source_roots().roots);
            continue;
        }
        if let Some(adapter) = crate::adapters::by_name(runtime) {
            roots.extend(adapter.roots(&home));
        }
    }
    roots
}

/// One structured watch line: JSON when the operator asked for it, otherwise
/// the timestamped key=value form the timer's logs are read in.
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
        println!("{ts} watch {kind}");
    } else {
        println!("{ts} watch {kind} {text}");
    }
}

/// Run one refresh step as a child of this process, so a crash in a step is a
/// non-zero status here rather than a dead watcher.
fn run_step(json: bool, command: &str) -> i32 {
    log(json, "run-start", &[("command", Value::String(command.to_string()))]);
    let executable =
        std::env::current_exe().unwrap_or_else(|_| PathBuf::from(env!("CARGO_BIN_NAME")));
    match std::process::Command::new(executable).arg(command).status() {
        Ok(status) => {
            let code = status.code().unwrap_or(1);
            log(
                json,
                "run-finish",
                &[
                    ("command", Value::String(command.to_string())),
                    ("status", Value::from(code)),
                ],
            );
            code
        }
        Err(error) => {
            log(
                json,
                "run-finish",
                &[
                    ("command", Value::String(command.to_string())),
                    ("error", Value::String(error.to_string())),
                ],
            );
            1
        }
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

/// Online freshness: recursively watch every supported source root, coalesce
/// changes over a quiet interval, then run the same refresh the external timer
/// runs (ingest, then export-oko) as child processes of this CLI. At most one
/// refresh runs and one more is queued; the writer lease inside ingest remains
/// the backstop against any other writer. This is a long-running foreground
/// process: launchd or systemd should KeepAlive it.
pub fn watch(rest: &[String]) -> Result<i32> {
    let parsed = parse_options("watch", rest, &["debounce"], &["json"])?;
    require_flags_only("watch", &parsed)?;
    let debounce_seconds = bounded_integer(
        parsed.value("debounce"),
        "--debounce",
        DEFAULT_DEBOUNCE as i64,
        MAX_LIMIT,
    )? as u64;
    let json = parsed.flag("json");
    let roots = watch_roots();
    if roots.is_empty() {
        return Err(Error("watch found no supported source roots on this machine".into()));
    }
    let debounce = Duration::from_millis(debounce_seconds * SECOND_MS);
    let (sender, receiver) = channel();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        if event.is_ok() {
            let _ = sender.send(());
        }
    })
    .map_err(|error| Error(format!("watch could not start: {error}")))?;
    for root in &roots {
        watcher
            .watch(root, RecursiveMode::Recursive)
            .map_err(|error| Error(format!("watch could not watch {}: {error}", root.display())))?;
    }
    install_stop_handlers();

    let running = Arc::new(AtomicBool::new(false));
    let queued = Arc::new(AtomicBool::new(false));
    let mut workers: VecDeque<thread::JoinHandle<()>> = VecDeque::new();
    let mut pending: u64 = 0;
    let mut deadline: Option<Instant> = None;
    log(
        json,
        "start",
        &[
            ("roots", Value::from(roots.len())),
            ("debounceSeconds", Value::from(debounce_seconds)),
        ],
    );
    while !STOP.load(Ordering::SeqCst) {
        let wait = deadline
            .map(|at| at.saturating_duration_since(Instant::now()))
            .unwrap_or(TICK)
            .min(TICK);
        match receiver.recv_timeout(wait) {
            Ok(()) => {
                pending += 1;
                deadline = Some(Instant::now() + debounce);
                continue;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        let Some(at) = deadline else {
            continue;
        };
        if Instant::now() < at {
            continue;
        }
        deadline = None;
        let batch = pending;
        pending = 0;
        log(json, "batch", &[("events", Value::from(batch))]);
        if running.load(Ordering::SeqCst) {
            // One run in flight, one queued: further batches while that queued
            // run is still pending collapse into it.
            if !queued.swap(true, Ordering::SeqCst) {
                log(json, "queued", &[("events", Value::from(batch))]);
            }
            continue;
        }
        running.store(true, Ordering::SeqCst);
        let (running_flag, queued_flag) = (Arc::clone(&running), Arc::clone(&queued));
        workers.push_back(thread::spawn(move || {
            loop {
                if run_step(json, "ingest") == 0 {
                    run_step(json, "export-oko");
                }
                if !queued_flag.swap(false, Ordering::SeqCst) {
                    break;
                }
            }
            running_flag.store(false, Ordering::SeqCst);
        }));
        while workers.front().is_some_and(thread::JoinHandle::is_finished) {
            if let Some(worker) = workers.pop_front() {
                let _ = worker.join();
            }
        }
    }
    for root in &roots {
        let _ = watcher.unwatch(root);
    }
    Ok(0)
}
