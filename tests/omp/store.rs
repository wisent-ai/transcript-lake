//! The OMP store a journey adopts from: OMP's session layout under a home,
//! the project a case copies its sessions into, and the canonical rows an
//! adoption wrote.
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
};

/// OMP's layout under the home: `.omp/agent/sessions/<project>/<file>.jsonl`.
const SESSIONS: &[&str] = &[".omp", "agent", "sessions"];

/// The project a case copies its sessions into.
pub const PROJECT: &str = "-Documents-case";

/// OMP's session root under `home`.
pub fn under(home: &Path) -> PathBuf {
    SESSIONS
        .iter()
        .fold(home.to_path_buf(), |path, part| path.join(part))
}

/// The partition rows the adoption wrote.
pub fn rows(lake: &Path) -> Vec<Value> {
    let mut pending = vec![lake.join("events")];
    let mut rows = Vec::new();
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).expect("read canonical partition directory") {
            let path = entry.expect("read partition entry").path();
            if path.is_dir() {
                pending.push(path);
            } else if path
                .extension()
                .is_some_and(|extension| extension == "ndjson")
            {
                rows.extend(
                    fs::read_to_string(&path)
                        .expect("read canonical partition")
                        .lines()
                        .map(|line| serde_json::from_str::<Value>(line).expect("canonical event")),
                );
            }
        }
    }
    rows
}
