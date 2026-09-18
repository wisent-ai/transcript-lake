//! The speaker on an exported row, through the real binary and a real ingest.
//!
//! The write gate that demands a recorded justification for a new test reads
//! Oko's index, opens the transcript file it names, and accepts the operator's
//! quote only from a line that says a user wrote it and carries the words
//! under `content`. Every file Oko indexes is one of these projections, so a
//! projection naming the speaker only in `event_type` made that gate unusable
//! for every session on the machine. These journeys drive the product the way
//! that gate reads it: a scratch HOME holds one omp transcript, the product
//! adopts it into a scratch Lake, and the exported rows are asked who spoke.
//!
//! Nothing here touches the operator's own Lake or home: every path lives
//! under Cargo's target directory for this test binary.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

const SESSION: &str = "01a0882d-0000-7000-8000-000000000002";
/// The operator's words in the seeded transcript: what a quote check looks for.
const ASKED: &str = "zainstaluj to wydanie";

fn scratch(label: &str) -> PathBuf {
    let root = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("oko-role-{label}-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&root).expect("create the scratch root");
    root
}

/// One omp transcript with both speakers and one tool call, in the line shapes
/// the omp adapter reads.
fn seed_home(root: &Path) -> PathBuf {
    let home = root.join("home");
    let sessions = home
        .join(".omp")
        .join("agent")
        .join("sessions")
        .join("-Users-operator-work");
    fs::create_dir_all(&sessions).expect("create the omp session root");
    let transcript = sessions.join(format!("2026-09-12T06-00-00-000Z_{SESSION}.jsonl"));
    let lines = [
        format!(
            "{{\"type\":\"session\",\"id\":\"{SESSION}\",\"cwd\":\"/Users/operator/work\",\"version\":\"0.1.0\"}}"
        ),
        format!(
            "{{\"type\":\"message\",\"id\":\"m1\",\"timestamp\":\"2026-09-12T06:00:01.000Z\",\
             \"message\":{{\"role\":\"user\",\"content\":[{{\"type\":\"text\",\"text\":\"{ASKED}\"}}]}}}}"
        ),
        "{\"type\":\"message\",\"id\":\"m2\",\"timestamp\":\"2026-09-12T06:00:02.000Z\",\
         \"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"installed it\"}]}}"
            .to_string(),
        "{\"type\":\"message\",\"id\":\"m3\",\"timestamp\":\"2026-09-12T06:00:03.000Z\",\
         \"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"toolCall\",\"id\":\"t1\",\
         \"name\":\"bash\",\"arguments\":{\"command\":\"true\"}}]}}"
            .to_string(),
    ];
    fs::write(&transcript, format!("{}\n", lines.join("\n"))).expect("write the transcript");
    home
}

fn lake(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_transcript-lake"))
        .args(args)
        .env("HOME", home)
        .output()
        .expect("run the product")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).to_string()
}

/// The archive these reads run against: adopted through the product's own
/// ingestion boundary, never by writing its files directly.
fn adopted(label: &str) -> (PathBuf, PathBuf) {
    let root = scratch(label);
    let home = seed_home(&root);
    let data_dir = root.join("lake");
    let source_root = home
        .join(".omp")
        .join("agent")
        .join("sessions")
        .join("-Users-operator-work");
    let adopt = lake(
        &home,
        &[
            "--data-dir",
            data_dir.to_str().expect("utf-8 data dir"),
            "adopt",
            "--source",
            "omp",
            "--root",
            source_root.to_str().expect("utf-8 source root"),
            "--json",
        ],
    );
    assert!(
        adopt.status.success(),
        "adopt refused: {}{}",
        text(&adopt.stdout),
        text(&adopt.stderr)
    );
    (home, data_dir)
}

fn session_file(data_dir: &Path) -> PathBuf {
    let runtime_dir = data_dir.join("exports").join("oko").join("runtime=omp");
    let mut files: Vec<PathBuf> = fs::read_dir(&runtime_dir)
        .expect("read the exported runtime directory")
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "jsonl"))
        .collect();
    files.sort();
    assert_eq!(files.len(), 1, "one adopted session, one exported file");
    files.remove(0)
}

/// Every exported row of the adopted session, in file order.
fn exported_rows(data_dir: &Path) -> Vec<Value> {
    let content = fs::read_to_string(session_file(data_dir)).expect("read the exported session");
    content
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("exported row is JSON"))
        .collect()
}

fn row_of<'a>(rows: &'a [Value], event_type: &str) -> &'a Value {
    rows.iter()
        .find(|row| row["event_type"] == event_type)
        .unwrap_or_else(|| panic!("no exported {event_type} row"))
}

#[test]
fn the_exported_row_names_who_spoke() {
    let (_home, data_dir) = adopted("speakers");
    let rows = exported_rows(&data_dir);

    assert_eq!(row_of(&rows, "user")["role"], serde_json::json!("user"));
    assert_eq!(
        row_of(&rows, "assistant")["role"],
        serde_json::json!("assistant")
    );
    assert_eq!(
        row_of(&rows, "tool_call")["role"],
        Value::Null,
        "a tool call has no speaker to name"
    );
    assert_eq!(
        row_of(&rows, "user")["content"],
        serde_json::json!(ASKED),
        "an operator turn carries its words where a quote check reads them"
    );
}

/// On 2026-09-18 every omp `tool_call` row in the operator's Lake carried an
/// empty `text`: the adapter kept the tool's name and call id and dropped its
/// arguments, so Oko's one-off repair detector, which reads the command an
/// agent ran, saw nothing for any omp session - including the `skarbiec grant
/// ensure` the operator had just asked about. The claude adapter had always
/// written the input; the row now says what was run for omp too.
#[test]
fn the_exported_tool_call_carries_what_was_run() {
    let (_home, data_dir) = adopted("arguments");
    let rows = exported_rows(&data_dir);
    let call = row_of(&rows, "tool_call");
    assert_eq!(call["tool_name"], serde_json::json!("bash"));
    assert_eq!(call["extra"]["call_id"], serde_json::json!("t1"));
    let text = call["text"].as_str().expect("a tool call row carries text");
    let arguments: Value =
        serde_json::from_str(text).expect("the text is the call's arguments as JSON");
    assert_eq!(
        arguments,
        serde_json::json!({"command": "true"}),
        "a reader of the projection sees what the agent ran, not only that it ran something"
    );
}

/// The check the write gate performs, against the file Oko would index: find
/// the line carrying the quote, and accept it only when that line says a user
/// wrote it and holds the words in `content`. This failed for every session
/// until the row carried both.
#[test]
fn a_quote_check_finds_the_operator_turn() {
    let (_home, data_dir) = adopted("quote");
    let content = fs::read_to_string(session_file(&data_dir)).expect("read the exported session");

    let mut accepted = false;
    for line in content.lines() {
        if !line.contains(ASKED) {
            continue;
        }
        let row: Value = serde_json::from_str(line).expect("exported row is JSON");
        let message = row.get("message").filter(|value| value.is_object());
        let role = message
            .and_then(|message| message.get("role"))
            .or_else(|| row.get("role"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        if role != "user" {
            continue;
        }
        let spoken = message.unwrap_or(&row).get("content");
        let texts: Vec<String> = match spoken {
            Some(Value::String(said)) => vec![said.clone()],
            Some(Value::Array(parts)) => parts
                .iter()
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .map(str::to_string)
                .collect(),
            _ => Vec::new(),
        };
        if texts.iter().any(|said| said.contains(ASKED)) {
            accepted = true;
        }
    }
    assert!(
        accepted,
        "no line carrying the quote is readable as an operator turn: {content}"
    );
}

/// The full rebuild writes the same rows as the ingest-time projection, so a
/// machine repaired with `rebuild-oko` is readable by the same check.
#[test]
fn a_rebuild_keeps_the_speaker() {
    let (home, data_dir) = adopted("rebuild");
    let rebuilt = lake(
        &home,
        &[
            "--data-dir",
            data_dir.to_str().expect("utf-8 data dir"),
            "rebuild-oko",
        ],
    );
    assert!(
        rebuilt.status.success(),
        "rebuild-oko refused: {}{}",
        text(&rebuilt.stdout),
        text(&rebuilt.stderr)
    );
    let rows = exported_rows(&data_dir);
    assert_eq!(row_of(&rows, "user")["role"], serde_json::json!("user"));
    assert_eq!(row_of(&rows, "user")["content"], serde_json::json!(ASKED));
}
