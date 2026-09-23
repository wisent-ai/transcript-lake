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
pub(super) const TICK: Duration = Duration::from_millis(250);

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
pub(super) fn source_roots(data_dir: &Path) -> Result<Vec<PathBuf>> {
    if let Some(selected) = crate::sources::selected_source(data_dir)? {
        if !selected.root.is_dir() {
            return Err(Error(format!(
                "selected source root is unavailable: {}",
                selected.root.display()
            )));
        }
        return Ok(vec![selected.root]);
    }
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
    Ok(roots)
}

/// One structured stream line: JSON when requested, otherwise timestamped

mod service;
mod predecessor;

pub use service::stream;
