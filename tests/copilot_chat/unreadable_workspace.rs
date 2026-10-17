//! Real CLI journey: a VS Code chat session whose workspace's
//! `workspace.json` is not JSON is adopted with its messages, and the
//! unreadable file is named on stderr with the fact that its sessions get no
//! project - it used to be dropped without a word, so the sessions were filed
//! under no project in silence. A workspace whose file names a local folder
//! files the same session under that folder. Each store is a VS Code user
//! directory under the journey's home, at `.config/Code/User`, where Copilot
//! Chat discovery finds the root `transcript-lake adopt --root` names; the
//! journey, its inputs and outputs are retained under the package's build
//! directory by `evidence`.
#[path = "../harnesses/evidence.rs"]
mod evidence;

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

const SESSION: &str = "copilot-unreadable-workspace";
const PROMPT: &str = "the question asked in the workspace";
const FOLDER: &str = "/work/copilot-project";

/// A workspace holding one chat session, its `workspace.json` written as
/// `workspace`; answers the session directory adopt reads.
fn store(home: &Path, workspace: &str) -> PathBuf {
    let sessions = home
        .join(".config")
        .join("Code")
        .join("User")
        .join("workspaceStorage")
        .join("journey-workspace")
        .join("chatSessions");
    fs::create_dir_all(&sessions).expect("the chat sessions directory is created");
    fs::write(
        sessions
            .parent()
            .expect("a workspace directory")
            .join("workspace.json"),
        workspace,
    )
    .expect("the workspace file is written");
    fs::write(
        sessions.join(format!("{SESSION}.json")),
        json!({
            "sessionId": SESSION,
            "requests": [{
                "requestId": "first-request",
                "message": {"text": PROMPT},
                "response": [{"value": "the answer given"}],
            }],
        })
        .to_string(),
    )
    .expect("the chat session is written");
    sessions
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

/// Adopts the store under `home` into `home/lake`; answers the command's
/// stderr and the prompt events it persisted for the session.
fn adopt(home: &Path, sessions: &Path, name: &str) -> (String, Vec<Value>) {
    let lake = home.join("lake");
    let formats = evidence::no_secret_classes(home);
    let output = evidence::run(
        home,
        name,
        &[
            "--data-dir",
            lake.to_str().expect("UTF-8 Lake path"),
            "adopt",
            "--source",
            "copilot-chat",
            "--root",
            sessions.to_str().expect("UTF-8 native root"),
            "--json",
        ],
        &[
            ("HOME", home),
            ("TRANSCRIPT_LAKE_SECRET_FORMATS", formats.as_path()),
        ],
    );
    let said = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(output.status.success(), "adopt failed: {said}");
    let prompts = adopted(&lake)
        .into_iter()
        .filter(|event| event["session_id"] == SESSION && event["event_type"] == "user")
        .collect();
    (said, prompts)
}

#[test]
fn an_unreadable_workspace_file_is_named_and_its_session_is_adopted() {
    let home = evidence::directory();
    let sessions = store(&home, "{\"folder\": \"file:///work/cut off");
    let (said, prompts) = adopt(&home, &sessions, "adopt-unreadable-workspace");
    let workspace = sessions
        .parent()
        .expect("a workspace directory")
        .join("workspace.json");
    assert!(
        said.contains("copilot chat:")
            && said.contains(&workspace.display().to_string())
            && said.contains("is not a JSON document")
            && said.contains("its sessions get no project"),
        "the unreadable workspace file was not named: {said}"
    );
    assert!(
        prompts.iter().any(|event| event["text"] == PROMPT),
        "the session was not adopted: {prompts:?}"
    );
    assert!(
        prompts.iter().all(|event| event["project"].is_null()),
        "a project was invented for an unreadable workspace: {prompts:?}"
    );
}

#[test]
fn a_readable_workspace_file_files_its_session_under_the_folder() {
    let home = evidence::directory();
    let sessions = store(
        &home,
        &json!({"folder": format!("file://{FOLDER}")}).to_string(),
    );
    let (said, prompts) = adopt(&home, &sessions, "adopt-readable-workspace");
    assert!(
        !said.contains("copilot chat:"),
        "a readable workspace file was reported: {said}"
    );
    assert!(
        !prompts.is_empty() && prompts.iter().all(|event| event["project"] == FOLDER),
        "the session was not filed under its workspace folder: {prompts:?}"
    );
}
