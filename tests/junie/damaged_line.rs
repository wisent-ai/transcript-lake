//! Real CLI journey: a Junie transcript with a complete line that is not JSON
//! is adopted with every readable event, and the damaged line is named on
//! stderr with its file - it used to vanish, so the transcript read as a
//! shorter one. The store is a Junie sessions directory written where
//! `transcript-lake adopt --root` reads it; the journey, its inputs and
//! outputs are retained under the package's build directory by `evidence`.
#[path = "../harnesses/evidence.rs"]
mod evidence;

use std::fs;
use std::path::Path;

use serde_json::Value;

const SESSION: &str = "junie-damaged-line";

fn store(work: &Path) -> std::path::PathBuf {
    let root = work.join("junie").join("sessions");
    let session = root.join(SESSION);
    fs::create_dir_all(&session).expect("the Junie session directory is created");
    fs::write(
        root.join("index.jsonl"),
        format!("{{\"sessionId\":\"{SESSION}\",\"projectDir\":\"/work/lake-test\"}}\n"),
    )
    .expect("the session index is written");
    fs::write(
        session.join("events.jsonl"),
        concat!(
            "{\"kind\":\"UserPromptEvent\",\"timestampMs\":1785600000000,\"prompt\":\"first prompt before the damage\"}\n",
            "{\"kind\":\"UserPromptEvent\",\"prompt\":\"cut off mid-record\n",
            "{\"kind\":\"UserPromptEvent\",\"timestampMs\":1785600060000,\"prompt\":\"second prompt after the damage\"}\n",
        ),
    )
    .expect("the transcript is written");
    root
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
fn a_damaged_line_is_named_and_the_rest_is_adopted() {
    let work = evidence::directory();
    let root = store(&work);
    let lake = work.join("lake");
    let output = evidence::run(
        &work,
        "adopt-damaged-line",
        &[
            "--data-dir",
            lake.to_str().expect("UTF-8 Lake path"),
            "adopt",
            "--source",
            "junie",
            "--root",
            root.to_str().expect("UTF-8 native root"),
            "--json",
        ],
        &[],
    );
    let said = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "adopt failed: {said}");
    let transcript = root.join(SESSION).join("events.jsonl");
    assert!(
        said.contains("is not JSON and carries no record")
            && said.contains(&transcript.display().to_string()),
        "the damaged line was not named: {said}"
    );
    let prompts: Vec<String> = adopted(&lake)
        .iter()
        .filter(|event| event["session_id"] == SESSION && event["event_type"] == "user")
        .filter_map(|event| event["text"].as_str().map(str::to_string))
        .collect();
    assert!(
        prompts.contains(&"first prompt before the damage".to_string())
            && prompts.contains(&"second prompt after the damage".to_string()),
        "the readable events were not adopted: {prompts:?}"
    );
}
