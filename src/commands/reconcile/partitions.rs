//! One runtime's canonical source partitions, read from a Lake root and
//! compared with the partitions a fresh replay of the retained originals
//! wrote into a staging root beside it.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::{self, File};
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::oko_export::fingerprint;
use crate::util::{Error, Result};

/// A partition's files by the date directory that holds each.
type Files = BTreeMap<String, PathBuf>;

/// What a replay of the retained originals says about one source partition.
pub(super) enum Verdict {
    /// The replay wrote the same bytes.
    Unchanged,
    /// The replay projects the source differently: `removed` recorded rows no
    /// longer follow from it and `added` rows do that were never recorded.
    Changed { removed: usize, added: usize },
    /// No retained original on this machine reproduces the partition, so no
    /// row of it can be proved or corrected; it stays as it is.
    Unprovable,
    /// The partition holds rows recorded on other machines (or on none), so
    /// this machine's original cannot stand for all of it.
    Refused { machines: BTreeSet<String> },
}

pub(super) struct Partition {
    pub(super) name: String,
    pub(super) sessions: BTreeSet<String>,
    pub(super) rows: usize,
    pub(super) verdict: Verdict,
    current: Files,
    staged: Files,
}

fn listing(path: &Path) -> Result<Option<fs::ReadDir>> {
    match fs::read_dir(path) {
        Ok(entries) => Ok(Some(entries)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(Error(format!(
            "cannot list canonical partitions at {}: {error}",
            path.display()
        ))),
    }
}

fn runtime_root(root: &Path, runtime: &str) -> PathBuf {
    root.join("events").join(format!("runtime={runtime}"))
}

/// Every `.ndjson` source partition under one runtime of a Lake root.
fn layout(root: &Path, runtime: &str) -> Result<BTreeMap<String, Files>> {
    let mut layout: BTreeMap<String, Files> = BTreeMap::new();
    let base = runtime_root(root, runtime);
    let Some(dates) = listing(&base)? else {
        return Ok(layout);
    };
    for date in dates {
        let date =
            date.map_err(|error| Error(format!("cannot list {}: {error}", base.display())))?;
        let kind = date.file_type().map_err(|error| {
            Error(format!(
                "cannot read the type of {}: {error}",
                date.path().display()
            ))
        })?;
        if !kind.is_dir() {
            continue;
        }
        let directory = date.path();
        let Some(files) = listing(&directory)? else {
            continue;
        };
        let date_name = date.file_name().to_string_lossy().to_string();
        for file in files {
            let file = file
                .map_err(|error| Error(format!("cannot list {}: {error}", directory.display())))?;
            let name = file.file_name().to_string_lossy().to_string();
            if name.ends_with(".ndjson") {
                layout
                    .entry(name)
                    .or_default()
                    .insert(date_name.clone(), file.path());
            }
        }
    }
    Ok(layout)
}

fn rows(files: &Files) -> Result<Vec<Value>> {
    let mut rows = Vec::new();
    for path in files.values() {
        let text = fs::read_to_string(path).map_err(|error| {
            Error(format!(
                "cannot read canonical partition {}: {error}",
                path.display()
            ))
        })?;
        for line in text.lines().filter(|line| !line.trim().is_empty()) {
            let row = serde_json::from_str(line).map_err(|error| {
                Error(format!(
                    "canonical partition {} holds a line that is not an event ({error}): {line}",
                    path.display()
                ))
            })?;
            rows.push(row);
        }
    }
    Ok(rows)
}

fn read_partition(path: &Path) -> Result<Vec<u8>> {
    fs::read(path).map_err(|error| {
        Error(format!(
            "cannot read canonical partition {}: {error}",
            path.display()
        ))
    })
}

fn same_bytes(current: &Files, staged: &Files) -> Result<bool> {
    if !current.keys().eq(staged.keys()) {
        return Ok(false);
    }
    for (held, fresh) in current.values().zip(staged.values()) {
        if read_partition(held)? != read_partition(fresh)? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Rows recorded that the replay no longer yields, and rows it yields that
/// were never recorded, counted by canonical occurrence.
fn difference(held: &[Value], fresh: &[Value], runtime: &str) -> (usize, usize) {
    let mut occurrences: HashMap<String, Vec<()>> = HashMap::new();
    for row in held {
        occurrences
            .entry(fingerprint(row, runtime))
            .or_default()
            .push(());
    }
    let added = fresh
        .iter()
        .filter(|row| {
            occurrences
                .get_mut(&fingerprint(row, runtime))
                .and_then(Vec::pop)
                .is_none()
        })
        .count();
    let removed = occurrences.values().map(Vec::len).sum();
    (removed, added)
}

/// Every source partition of `runtime` in either root, judged.
pub(super) fn compare(
    data_dir: &Path,
    staging: &Path,
    runtime: &str,
    machine: &str,
) -> Result<Vec<Partition>> {
    let mut current = layout(data_dir, runtime)?;
    let mut staged = layout(staging, runtime)?;
    let names: BTreeSet<String> = current.keys().chain(staged.keys()).cloned().collect();
    let mut partitions = Vec::with_capacity(names.len());
    for name in names {
        let current = match current.remove(&name) {
            Some(files) => files,
            None => Files::new(),
        };
        let staged = match staged.remove(&name) {
            Some(files) => files,
            None => Files::new(),
        };
        let held = rows(&current)?;
        let fresh = rows(&staged)?;
        let sessions = held
            .iter()
            .chain(&fresh)
            .filter_map(|row| row.get("session_id").and_then(Value::as_str))
            .map(str::to_string)
            .collect();
        let machines: BTreeSet<String> = held
            .iter()
            .filter_map(|row| match row.get("machine") {
                Some(Value::String(recorded)) if recorded == machine => None,
                Some(Value::String(recorded)) => Some(recorded.clone()),
                _ => Some("no recorded machine".to_string()),
            })
            .collect();
        let verdict = if staged.is_empty() {
            Verdict::Unprovable
        } else if !machines.is_empty() {
            Verdict::Refused { machines }
        } else if same_bytes(&current, &staged)? {
            Verdict::Unchanged
        } else {
            let (removed, added) = difference(&held, &fresh, runtime);
            Verdict::Changed { removed, added }
        };
        partitions.push(Partition {
            name,
            sessions,
            rows: held.len(),
            verdict,
            current,
            staged,
        });
    }
    Ok(partitions)
}

impl Partition {
    /// Put the replayed files in place of this partition's: each date file
    /// is renamed over its predecessor within the same Lake root, and a date
    /// the replay no longer writes is removed. Rerunning after an interrupted
    /// apply completes it, since every file is replaced whole.
    pub(super) fn apply(&self, data_dir: &Path, runtime: &str) -> Result<()> {
        let base = runtime_root(data_dir, runtime);
        for (date, staged) in &self.staged {
            let directory = base.join(date);
            let target = directory.join(&self.name);
            let failure = |error: std::io::Error| {
                Error(format!(
                    "cannot replace canonical partition {} with the replayed {}: {error}",
                    target.display(),
                    staged.display()
                ))
            };
            fs::create_dir_all(&directory).map_err(failure)?;
            fs::rename(staged, &target).map_err(failure)?;
            File::open(&directory)
                .and_then(|handle| handle.sync_all())
                .map_err(failure)?;
        }
        for (date, held) in &self.current {
            if !self.staged.contains_key(date) {
                fs::remove_file(held).map_err(|error| {
                    Error(format!(
                        "cannot remove canonical partition {}, which the replay no longer writes: {error}",
                        held.display()
                    ))
                })?;
            }
        }
        Ok(())
    }
}
