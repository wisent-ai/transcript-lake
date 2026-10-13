//! An adopted OMP store keeps each assistant message's usage whole: the
//! fresh input, cache write and cache read parts land in `extra`, `tokens_in`
//! is their sum, and the credential the call was billed to travels with
//! them. The store is the machine's own newest OMP session (`OMP_SESSIONS`,
//! or `~/.omp/agent/sessions`), copied read-only into a home of the case's
//! own and adopted from there.
mod evidence;
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
};

/// OMP's layout under the home: `.omp/agent/sessions/<project>/<file>.jsonl`.
const SESSIONS: &[&str] = &[".omp", "agent", "sessions"];
const PROJECT: &str = "-Documents-case";

fn under(home: &Path) -> PathBuf {
    SESSIONS
        .iter()
        .fold(home.to_path_buf(), |path, part| path.join(part))
}

fn sessions_root() -> PathBuf {
    match std::env::var_os("OMP_SESSIONS") {
        Some(path) => PathBuf::from(path),
        None => under(Path::new(
            &std::env::var_os("HOME").expect("HOME names the home"),
        )),
    }
}

/// The newest session file under `root`, by its last write.
fn newest_file(root: &Path) -> PathBuf {
    let mut newest: Option<(std::time::SystemTime, PathBuf)> = None;
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let entries = fs::read_dir(&directory);
        assert!(entries.is_ok(), "{} cannot be listed", directory.display());
        for entry in entries.expect("listed") {
            let entry = entry.expect("an entry");
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if path
                .extension()
                .is_some_and(|extension| extension == "jsonl")
            {
                let written = entry
                    .metadata()
                    .expect("metadata")
                    .modified()
                    .expect("a write time");
                if newest.as_ref().is_none_or(|(at, _)| written > *at) {
                    newest = Some((written, path));
                }
            }
        }
    }
    assert!(
        newest.is_some(),
        "no OMP session file under {}",
        root.display()
    );
    newest.expect("the newest file").1
}

/// The usage OMP recorded on an assistant message, as its `usage` object
/// names the parts, with the credential the call was billed to.
struct Usage {
    input: i64,
    output: i64,
    cache_write: i64,
    cache_read: i64,
    credential: i64,
}

impl Usage {
    /// The usage of `record` when it is an assistant message carrying a
    /// credential and every part; any other record carries none.
    fn of(record: &Value) -> Option<Usage> {
        let message = &record["message"];
        if message["role"] != "assistant" {
            return None;
        }
        let usage = &message["usage"];
        Some(Usage {
            input: usage["input"].as_i64()?,
            output: usage["output"].as_i64()?,
            cache_write: usage["cacheWrite"].as_i64()?,
            cache_read: usage["cacheRead"].as_i64()?,
            credential: message["credentialId"].as_i64()?,
        })
    }

    fn whole(&self) -> i64 {
        self.input + self.cache_write + self.cache_read
    }
}

/// The partition rows the adoption wrote.
fn rows(lake: &Path) -> Vec<Value> {
    let events = lake.join("events");
    let listed = fs::read_dir(&events);
    assert!(listed.is_ok(), "{} cannot be listed", events.display());
    let mut rows = Vec::new();
    for entry in listed.expect("listed") {
        let path = entry.expect("an entry").path();
        let text = fs::read_to_string(&path);
        assert!(text.is_ok(), "{} cannot be read", path.display());
        rows.extend(
            text.expect("read")
                .lines()
                .map(|line| serde_json::from_str::<Value>(line).expect("a row")),
        );
    }
    rows
}

#[test]
fn omp_usage_parts_and_credential_reach_the_partition() {
    let source = newest_file(&sessions_root());
    let text = fs::read_to_string(&source);
    assert!(text.is_ok(), "{} cannot be read", source.display());
    let text = text.expect("read");
    let measured = text
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find_map(|record| Usage::of(&record).map(|usage| (record["timestamp"].clone(), usage)));
    assert!(
        measured.is_some(),
        "{} holds no billed assistant message to measure",
        source.display()
    );
    let (stamp, usage) = measured.expect("a billed message");
    let work = evidence::directory();
    let home = work.join("home");
    let project = under(&home).join(PROJECT);
    fs::create_dir_all(&project).expect("the case's project directory is made");
    let copied = project.join(source.file_name().expect("the source has a name"));
    fs::write(&copied, &text).expect("the session file is copied");
    let lake = work.join("lake");
    let args = [
        "--data-dir",
        lake.to_str().expect("UTF-8 lake path"),
        "adopt",
        "--source",
        "omp",
        "--root",
        project.to_str().expect("UTF-8 project path"),
        "--json",
    ];
    let adopted = evidence::run(&work, "adopt", &args, &[("HOME", home.as_path())]);
    assert!(
        adopted.status.success(),
        "{}",
        String::from_utf8_lossy(&adopted.stderr)
    );
    let rows = rows(&lake);
    let row = rows.iter().find(|row| {
        row["role"] == "assistant"
            && row["ts"] == stamp
            && row["extra"]["credential_id"].as_i64() == Some(usage.credential)
    });
    assert!(
        row.is_some(),
        "no assistant row dated {stamp} billed to {}",
        usage.credential
    );
    let row = row.expect("the measured row");
    assert_eq!(row["tokens_in"].as_i64(), Some(usage.whole()), "{row}");
    assert_eq!(row["tokens_out"].as_i64(), Some(usage.output), "{row}");
    assert_eq!(
        row["extra"]["input_non_cached_tokens"].as_i64(),
        Some(usage.input),
        "{row}"
    );
    assert_eq!(
        row["extra"]["cache_creation_tokens"].as_i64(),
        Some(usage.cache_write),
        "{row}"
    );
    assert_eq!(
        row["extra"]["cache_read_tokens"].as_i64(),
        Some(usage.cache_read),
        "{row}"
    );
}
