//! Ingest, safe replay, and the online watch loop. Every one of them ends in
//! the same refresh the external timer runs — ingest, then the Oko export —
//! so a Lake is never left with events its derived artifacts do not know.
use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, RecvTimeoutError};
use std::time::{Duration, Instant};

use notify::{RecursiveMode, Watcher};
use serde_json::{Map, Value};

use crate::args::{
    bounded_integer, parse_options, require_flags_only, require_runtime, DEFAULT_DEBOUNCE,
    MAX_LIMIT,
};
use crate::ingest::{ingest as run_ingest, IngestOptions};
use crate::paths::{hook_source_roots, resolve_data_dir, SUMMARY_FILE};
use crate::util::{absolute, home_dir, now_iso, write_json, Error, Result};

const SECOND_MS: u64 = 1000;
/// How often the watch loop wakes to notice a signal while it is otherwise
/// idle.
const TICK: Duration = Duration::from_millis(250);
/// How long a burst of writes is gathered before it is read. One agent turn
/// appends several times in quick succession; this collapses that into one
/// pass without making the operator wait for a quiet interval.
const COALESCE: Duration = Duration::from_millis(250);

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

/// The directories to watch recursively.
///
/// Deliberately NOT `adapter.roots()`. Several adapters root themselves at a
/// per-working-directory subdirectory — omp and droid create one the first
/// time an agent runs somewhere new — and those are enumerated once. A batch
/// run re-listed them every time and so noticed a new project by accident; a
/// watcher started before that directory existed would never see it, and every
/// session in a new checkout would be invisible until someone restarted the
/// service. Watching the stable base above them covers the ones that exist and
/// the ones created later, and costs nothing extra: the notification carries
/// the path, and a path no adapter claims is skipped.
fn watch_roots() -> Vec<PathBuf> {
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

/// Read the named files, then let the export merge what they produced.
///
/// In process, not as child processes: spawning the CLI twice per burst pays
/// two process startups and, worse, hands the children no way to be told which
/// files moved — they would rediscover it by walking. A failure here is logged
/// and the loop continues, because the next write brings the same paths back
/// and the hourly timer sweeps anything that stays stuck.
fn refresh_paths(json: bool, data_dir: &Path, paths: &[PathBuf]) {
    log(json, "run-start", &[("command", Value::String("ingest".into()))]);
    match crate::ingest::ingest_paths(data_dir, paths) {
        Ok(summary) => {
            let files = summary.get("filesIngested").and_then(Value::as_u64).unwrap_or(0);
            log(
                json,
                "run-finish",
                &[
                    ("command", Value::String("ingest".into())),
                    ("files", Value::from(files)),
                    ("ms", summary.get("durationMs").cloned().unwrap_or(Value::Null)),
                ],
            );
            if files == 0 {
                // Nothing this adapter set owns was written — an editor's
                // scratch file, a lock, a directory touch. No partitions
                // changed, so the export has nothing to merge.
                return;
            }
        }
        Err(error) => {
            log(
                json,
                "run-finish",
                &[
                    ("command", Value::String("ingest".into())),
                    ("error", Value::String(error.to_string())),
                ],
            );
            return;
        }
    }
    log(json, "run-start", &[("command", Value::String("export-oko".into()))]);
    match crate::oko_export::export_oko(false, data_dir) {
        Ok(summary) => log(
            json,
            "run-finish",
            &[
                ("command", Value::String("export-oko".into())),
                ("sessions", summary.get("sessions").cloned().unwrap_or(Value::Null)),
                ("written", summary.get("written").cloned().unwrap_or(Value::Null)),
                ("ms", summary.get("durationMs").cloned().unwrap_or(Value::Null)),
            ],
        ),
        Err(error) => log(
            json,
            "run-finish",
            &[
                ("command", Value::String("export-oko".into())),
                ("error", Value::String(error.to_string())),
            ],
        ),
    }
}

/// Online freshness: watch every supported source root and, when a file under
/// one of them is written, read that file from where the cursor left it.
///
/// This used to discard the event, wait out a sixty-second quiet interval, and
/// then spawn `ingest` and `export-oko` as child processes — a full walk of
/// every root and a stat of every transcript on the machine, to rediscover the
/// one file the notification had already named. That is a batch refresh on a
/// trigger, and it is why a conversation could sit six hours outside the Lake.
///
/// Now the path travels with the event, a short window coalesces the burst of
/// writes one turn produces, and the ingest reads exactly those files in this
/// process. The export that follows is already incremental: it reads the bytes
/// appended to the partitions it just wrote and merges them into the sessions
/// they belong to.
///
/// The lease is taken per batch rather than held for the process lifetime, so
/// the hourly timer stays a working backstop for anything written while this
/// process was down.
pub fn watch(rest: &[String]) -> Result<i32> {
    let parsed = parse_options("watch", rest, &["debounce"], &["json"])?;
    require_flags_only("watch", &parsed)?;
    let debounce_ms = match parsed.value("debounce") {
        Some(_) => {
            bounded_integer(parsed.value("debounce"), "--debounce", DEFAULT_DEBOUNCE as i64, MAX_LIMIT)?
                as u64
                * SECOND_MS
        }
        None => COALESCE.as_millis() as u64,
    };
    let json = parsed.flag("json");
    let data_dir = resolve_data_dir(None);
    let roots = watch_roots();
    if roots.is_empty() {
        return Err(Error("watch found no supported source roots on this machine".into()));
    }
    let debounce = Duration::from_millis(debounce_ms);
    let (sender, receiver) = channel();
    let mut watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        // The paths are the point. A notification that says only "something
        // moved" forces the reader to go and find out what, which is the walk
        // this loop exists to avoid.
        if let Ok(event) = event {
            for path in event.paths {
                let _ = sender.send(path);
            }
        }
    })
    .map_err(|error| Error(format!("watch could not start: {error}")))?;
    for root in &roots {
        watcher
            .watch(root, RecursiveMode::Recursive)
            .map_err(|error| Error(format!("watch could not watch {}: {error}", root.display())))?;
    }
    install_stop_handlers();

    // One refresh at a time, and the paths that arrived while it ran are
    // carried into the next one rather than dropped.
    let mut pending: BTreeSet<PathBuf> = BTreeSet::new();
    let mut deadline: Option<Instant> = None;
    log(
        json,
        "start",
        &[
            ("roots", Value::from(roots.len())),
            ("coalesceMs", Value::from(debounce.as_millis() as u64)),
        ],
    );
    while !STOP.load(Ordering::SeqCst) {
        let wait = deadline
            .map(|at| at.saturating_duration_since(Instant::now()))
            .unwrap_or(TICK)
            .min(TICK);
        match receiver.recv_timeout(wait) {
            Ok(path) => {
                pending.insert(path);
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
        if pending.is_empty() {
            continue;
        }
        let batch: Vec<PathBuf> = pending.iter().cloned().collect();
        pending.clear();
        log(
            json,
            "batch",
            &[
                ("paths", Value::from(batch.len())),
                // The first path, so a batch that ingests nothing says what it
                // was looking at instead of leaving the operator to guess.
                ("first", Value::String(batch[0].display().to_string())),
            ],
        );
        refresh_paths(json, &data_dir, &batch);
    }
    for root in &roots {
        let _ = watcher.unwatch(root);
    }
    Ok(0)
}
