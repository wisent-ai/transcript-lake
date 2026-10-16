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

/// The newest discovered session file directly under a project of `root`.
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
                if directory == root {
                    pending.push(path);
                }
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

#[test]
fn omp_usage_parts_and_credential_reach_the_partition() {
    let source = newest_file(&sessions_root());
    let text = fs::read_to_string(&source);
    assert!(text.is_ok(), "{} cannot be read", source.display());
    let text = text.expect("read");
    let measured = text
        .lines()
        .map(|line| serde_json::from_str::<Value>(line).expect("valid native OMP JSON record"))
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

#[derive(serde::Deserialize)]
struct Authorship {
    authored_file: PathBuf,
    authored_session: String,
    authored_text: String,
    generated_file: PathBuf,
    generated_session: String,
    generated_text: String,
}

#[test]
#[ignore = "requires independently identified real OMP operator and advisor records"]
fn native_advisor_updates_are_not_operator_turns() {
    let manifest = std::env::var("LAKE_TEST_OMP_AUTHORSHIP").expect(
        "set LAKE_TEST_OMP_AUTHORSHIP to an independently observed native session expectation",
    );
    let body = fs::read(manifest).expect("read OMP authorship expectation");
    let expected: Authorship =
        serde_json::from_slice(&body).expect("decode OMP authorship expectation");
    assert!(
        !expected.authored_text.is_empty(),
        "name a real operator turn"
    );
    assert!(
        !expected.generated_text.is_empty(),
        "name a real advisor update"
    );
    let work = evidence::directory();
    fs::write(work.join("expectation.json"), body).expect("retain acceptance input");
    let home = work.join("home");
    let project = under(&home).join(PROJECT);
    fs::create_dir_all(&project).expect("create isolated native-source project");
    for source in [&expected.authored_file, &expected.generated_file] {
        let name = source.file_name().expect("native source file has a name");
        fs::copy(source, project.join(name))
            .expect("copy unchanged native transcript into isolated project");
    }
    let lake = work.join("lake");
    let adopted = evidence::run(
        &work,
        "adopt-authorship",
        &[
            "--data-dir",
            lake.to_str().expect("UTF-8 Lake path"),
            "adopt",
            "--source",
            "omp",
            "--root",
            project.to_str().expect("UTF-8 isolated project"),
            "--json",
        ],
        &[("HOME", home.as_path())],
    );
    assert!(
        adopted.status.success(),
        "{}",
        String::from_utf8_lossy(&adopted.stderr)
    );
    let events = rows(&lake);
    assert!(
        events.iter().any(|event| {
            event["session_id"] == expected.authored_session
                && event["event_type"] == "user"
                && event["text"]
                    .as_str()
                    .is_some_and(|text| text.contains(&expected.authored_text))
        }),
        "independently identified operator turn was not persisted as a user event"
    );
    assert!(
        events.iter().any(|event| {
            event["session_id"] == expected.generated_session
                && event["event_type"] == "meta"
                && event["extra"]["role"] == "user"
                && event["extra"]["synthetic"] == true
                && event["extra"]["attribution"] == "agent"
                && event["text"]
                    .as_str()
                    .is_some_and(|text| text.contains(&expected.generated_text))
        }),
        "native advisor update was not retained with its recorded provenance"
    );
    assert!(
        !events.iter().any(|event| {
            event["event_type"] == "user"
                && event["text"]
                    .as_str()
                    .is_some_and(|text| text.contains(&expected.generated_text))
        }),
        "agent-authored advisor update was attributed to the operator"
    );
    let shown = evidence::run(
        &work,
        "show-authorship",
        &[
            "--data-dir",
            lake.to_str().expect("UTF-8 Lake path"),
            "show",
            &expected.generated_session,
            "--include",
            "user,meta",
            "--json",
        ],
        &[],
    );
    assert!(
        shown.status.success(),
        "{}",
        String::from_utf8_lossy(&shown.stderr)
    );
    let report: Value = serde_json::from_slice(&shown.stdout).expect("decode actual show report");
    assert!(
        report["events"]
            .as_array()
            .expect("show events")
            .iter()
            .any(|event| {
                event["event_type"] == "meta"
                    && event["extra"]["synthetic"] == true
                    && event["extra"]["attribution"] == "agent"
                    && event["text"]
                        .as_str()
                        .is_some_and(|text| text.contains(&expected.generated_text))
            }),
        "show did not expose the advisor record and its native sender"
    );
    let refused = evidence::run(
        &work,
        "refused-authorship",
        &[
            "--data-dir",
            work.join("refused")
                .to_str()
                .expect("UTF-8 refusal destination"),
            "adopt",
            "--source",
            "omp",
            "--root",
            work.to_str().expect("UTF-8 unowned root"),
            "--json",
        ],
        &[],
    );
    assert!(
        !refused.status.success(),
        "an unowned source root must be refused"
    );
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("not a discovered"),
        "unexpected refusal: {}",
        String::from_utf8_lossy(&refused.stderr)
    );
}
