//! Real CLI journey: a current Codex rollout writes no `user_message` event;
//! the operator's turn is the completed item of type `UserMessage`. Adopting
//! such a rollout used to keep no operator turn at all, so a rebuild lost
//! every Codex prompt. The rollout is written where `transcript-lake adopt
//! --root` reads it; the journey, its inputs and outputs are retained under
//! the package's build directory by `evidence`.
#[path = "../harnesses/evidence.rs"]
mod evidence;

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

const SESSION: &str = "codex-completed-user-item";
const PROMPT: &str = "continue this work in parallel with the other agent";
const CONTEXT: &str = "<environment_context>cwd is /work/lake-test</environment_context>";
const AGENT: &str = "the agent's own completed words";

/// The fixture home and the Codex sessions root `adopt` discovers under it.
fn store(work: &Path) -> (PathBuf, PathBuf) {
    let home = work.join("home");
    let root = home.join(".codex").join("sessions");
    let day = root.join("2026").join("06").join("02");
    fs::create_dir_all(&day).expect("the Codex day directory is created");
    let lines = [
        format!("{{\"timestamp\":\"2026-06-02T03:35:11.090Z\",\"type\":\"session_meta\",\"payload\":{{\"id\":\"{SESSION}\",\"cwd\":\"/work/lake-test\"}}}}"),
        format!("{{\"timestamp\":\"2026-06-02T03:35:11.093Z\",\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"role\":\"user\",\"content\":[{{\"type\":\"input_text\",\"text\":\"{CONTEXT}\"}}]}}}}"),
        format!("{{\"timestamp\":\"2026-06-02T03:35:17.902Z\",\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"role\":\"user\",\"content\":[{{\"type\":\"input_text\",\"text\":\"{PROMPT}\"}}]}}}}"),
        format!("{{\"timestamp\":\"2026-06-02T03:35:17.903Z\",\"type\":\"event_msg\",\"payload\":{{\"type\":\"item_completed\",\"item\":{{\"type\":\"UserMessage\",\"id\":\"item-2\",\"content\":[{{\"type\":\"text\",\"text\":\"{PROMPT}\"}}]}}}}}}"),
        format!("{{\"timestamp\":\"2026-06-02T03:35:20.000Z\",\"type\":\"event_msg\",\"payload\":{{\"type\":\"item_completed\",\"item\":{{\"type\":\"AgentMessage\",\"id\":\"item-3\",\"content\":[{{\"type\":\"text\",\"text\":\"{AGENT}\"}}]}}}}}}"),
    ];
    fs::write(
        day.join(format!("rollout-2026-06-02T03-35-11-{SESSION}.jsonl")),
        lines.join("\n") + "\n",
    )
    .expect("the rollout is written");
    (home, root)
}

fn adopted(lake: &Path) -> Vec<Value> {
    let mut events = Vec::new();
    let mut directories = vec![lake.join("events")];
    while let Some(directory) = directories.pop() {
        for entry in fs::read_dir(&directory).expect("read a partition directory") {
            let path = entry.expect("read a partition entry").path();
            if path.is_dir() {
                directories.push(path);
            } else if path.extension().is_some_and(|ext| ext == "ndjson") {
                let text = fs::read_to_string(&path).expect("read a persisted partition");
                events.extend(
                    text.lines()
                        .map(|line| serde_json::from_str::<Value>(line).expect("canonical event")),
                );
            }
        }
    }
    events
}

#[test]
fn a_completed_user_item_is_the_operator_turn() {
    let work = evidence::directory();
    let (home, root) = store(&work);
    let lake = work.join("lake");
    let formats = evidence::no_secret_classes(&work);
    let output = evidence::run(
        &work,
        "adopt-completed-user-item",
        &[
            "--data-dir",
            lake.to_str().expect("UTF-8 Lake path"),
            "adopt",
            "--source",
            "codex",
            "--root",
            root.to_str().expect("UTF-8 native root"),
            "--json",
        ],
        &[
            ("HOME", home.as_path()),
            ("TRANSCRIPT_LAKE_SECRET_FORMATS", formats.as_path()),
        ],
    );
    let said = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "adopt failed: {said}");
    let users: Vec<(String, String)> = adopted(&lake)
        .iter()
        .filter(|event| event["session_id"] == SESSION && event["event_type"] == "user")
        .map(|event| {
            (
                event["ts"]
                    .as_str()
                    .expect("a user event has a time")
                    .to_string(),
                event["text"]
                    .as_str()
                    .expect("a user event has text")
                    .to_string(),
            )
        })
        .collect();
    let [(at, text)] = users.as_slice() else {
        panic!("exactly the one operator turn must be kept, without injected context or agent items: {users:?}");
    };
    assert_eq!(
        text, PROMPT,
        "the completed UserMessage item's words are the turn"
    );
    assert!(
        at.contains("03:35:17.903"),
        "the turn is dated by its completed item, got {at}"
    );
    assert!(
        text != CONTEXT && text != AGENT,
        "injected context or an agent item became the turn"
    );
}
