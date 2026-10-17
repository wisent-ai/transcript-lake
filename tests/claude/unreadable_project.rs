//! Real CLI journey: a Claude Code project directory that exists and cannot
//! be listed is named on stderr by `transcript-lake adopt`, with the cause,
//! instead of its sessions silently going unread; the sessions of the
//! readable project beside it are still adopted. The store is written where
//! `adopt --root` reads it; the journey, its inputs and outputs are retained
//! under the package's build directory by `evidence`.
#[path = "../harnesses/evidence.rs"]
mod evidence;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

const SESSION: &str = "claude-unreadable-project";
const ANSWER: &str = "the reply in the readable project";

/// A readable project with one session, and a project holding a session
/// whose directory is then made unreadable to everyone.
fn store(work: &Path) -> (PathBuf, PathBuf, PathBuf, fs::Permissions) {
    let home = work.join("home");
    let root = home.join(".claude").join("projects");
    let readable = root.join("-work-lake-test");
    fs::create_dir_all(&readable).expect("the readable project directory is created");
    let lines = [
        format!("{{\"type\":\"user\",\"sessionId\":\"{SESSION}\",\"cwd\":\"/work/lake-test\",\"timestamp\":\"2026-06-02T03:35:11.000Z\",\"message\":{{\"role\":\"user\",\"content\":\"read every project\"}}}}"),
        format!("{{\"type\":\"assistant\",\"sessionId\":\"{SESSION}\",\"timestamp\":\"2026-06-02T03:35:12.000Z\",\"message\":{{\"model\":\"claude-test\",\"content\":[{{\"type\":\"text\",\"text\":\"{ANSWER}\"}}]}}}}"),
    ];
    fs::write(
        readable.join(format!("{SESSION}.jsonl")),
        lines.join("\n") + "\n",
    )
    .expect("the transcript is written");
    let locked = root.join("-work-locked");
    fs::create_dir_all(&locked).expect("the locked project directory is created");
    fs::write(locked.join("hidden.jsonl"), "").expect("a session is placed in the locked project");
    let granted = fs::metadata(&locked)
        .expect("the locked project's permissions are read")
        .permissions();
    let removed = Command::new("chmod")
        .arg("a-rwx")
        .arg(&locked)
        .status()
        .expect("chmod runs");
    assert!(
        removed.success(),
        "chmod could not lock {}",
        locked.display()
    );
    (home, root, locked, granted)
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
fn an_unlistable_project_is_named_and_the_readable_one_adopted() {
    let work = evidence::directory();
    let (home, root, locked, granted) = store(&work);
    let lake = work.join("lake");
    let formats = evidence::no_secret_classes(&work);
    let output = evidence::run(
        &work,
        "adopt-claude-unreadable-project",
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
    // The retained journey directory stays removable.
    fs::set_permissions(&locked, granted).expect("the locked project gets its permissions back");
    let said = String::from_utf8_lossy(&output.stderr);
    assert!(
        said.contains(&format!("sessions: cannot list {}", locked.display())),
        "the unlistable project was not named: {said}"
    );
    assert!(output.status.success(), "adopt failed: {said}");
    let events = adopted(&lake);
    assert!(
        events
            .iter()
            .any(|event| event["session_id"] == SESSION && event["text"] == ANSWER),
        "the readable project's session was not adopted: {events:?}"
    );
}
