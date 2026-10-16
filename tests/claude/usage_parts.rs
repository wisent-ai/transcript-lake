//! Real CLI journey: a Claude Code transcript's usage is carried part by part
//! as the record reports it. A reply that reports only its output gets no
//! input count and no input parts, instead of an input of zero nobody
//! stated; a reply that reports a zero part keeps that zero. The transcript
//! is written where `transcript-lake adopt --root` reads it; the journey, its
//! inputs and outputs are retained under the package's build directory by
//! `evidence`.
#[path = "../harnesses/evidence.rs"]
mod evidence;

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

const SESSION: &str = "claude-usage-parts";
const OUTPUT_ONLY: &str = "the reply that reports only its output";
const EVERY_PART: &str = "the reply that reports every part";

fn store(work: &Path) -> (PathBuf, PathBuf) {
    let home = work.join("home");
    let root = home.join(".claude").join("projects");
    let project = root.join("-work-lake-test");
    fs::create_dir_all(&project).expect("the Claude project directory is created");
    let lines = [
        format!("{{\"type\":\"user\",\"sessionId\":\"{SESSION}\",\"cwd\":\"/work/lake-test\",\"timestamp\":\"2026-06-02T03:35:11.000Z\",\"message\":{{\"role\":\"user\",\"content\":\"count the tokens\"}}}}"),
        format!("{{\"type\":\"assistant\",\"sessionId\":\"{SESSION}\",\"timestamp\":\"2026-06-02T03:35:12.000Z\",\"message\":{{\"model\":\"claude-test\",\"content\":[{{\"type\":\"text\",\"text\":\"{OUTPUT_ONLY}\"}}],\"usage\":{{\"output_tokens\":7}}}}}}"),
        format!("{{\"type\":\"assistant\",\"sessionId\":\"{SESSION}\",\"timestamp\":\"2026-06-02T03:35:13.000Z\",\"message\":{{\"model\":\"claude-test\",\"content\":[{{\"type\":\"text\",\"text\":\"{EVERY_PART}\"}}],\"usage\":{{\"input_tokens\":3,\"cache_creation_input_tokens\":0,\"cache_read_input_tokens\":2,\"output_tokens\":1}}}}}}"),
    ];
    fs::write(
        project.join(format!("{SESSION}.jsonl")),
        lines.join("\n") + "\n",
    )
    .expect("the transcript is written");
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

fn reply<'a>(events: &'a [Value], text: &str) -> &'a Value {
    let found: Vec<&Value> = events
        .iter()
        .filter(|event| event["session_id"] == SESSION && event["text"] == text)
        .collect();
    let [event] = found.as_slice() else {
        panic!("exactly one persisted event must carry {text:?}: {found:?}");
    };
    event
}

#[test]
fn usage_parts_are_carried_as_reported() {
    let work = evidence::directory();
    let (home, root) = store(&work);
    let lake = work.join("lake");
    let formats = evidence::no_secret_classes(&work);
    let output = evidence::run(
        &work,
        "adopt-claude-usage-parts",
        &[
            "--data-dir",
            lake.to_str().expect("UTF-8 Lake path"),
            "adopt",
            "--source",
            "claude",
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
    let events = adopted(&lake);

    let output_only = reply(&events, OUTPUT_ONLY);
    assert_eq!(
        output_only["tokens_out"].to_string(),
        "7",
        "the reported output is carried: {output_only}"
    );
    assert!(
        output_only["tokens_in"].is_null(),
        "an input nobody reported became a count: {output_only}"
    );
    for part in [
        "input_non_cached_tokens",
        "cache_creation_tokens",
        "cache_read_tokens",
    ] {
        assert!(
            output_only["extra"].get(part).is_none(),
            "{part} was written although the record reports none: {output_only}"
        );
    }

    let every_part = reply(&events, EVERY_PART);
    assert_eq!(
        every_part["tokens_in"].to_string(),
        "5",
        "the input is the sum of the reported parts: {every_part}"
    );
    assert_eq!(
        every_part["extra"]["cache_creation_tokens"].to_string(),
        "0",
        "a reported zero is kept: {every_part}"
    );
}
