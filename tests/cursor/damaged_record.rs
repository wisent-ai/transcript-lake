//! Real CLI journey: a Cursor store whose chat holds one record that is not
//! JSON is adopted with every readable message, and the damaged record is
//! named on stderr with its key - it used to vanish from the conversation.
//! The store is a real `state.vscdb` written with the product's own SQLite
//! where `transcript-lake adopt --root` reads it; it is the vendor's store the
//! adapter reads, not product data. The journey, its inputs and outputs are
//! retained under the package's build directory by `evidence`.
#[path = "../harnesses/evidence.rs"]
mod evidence;

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

const CHAT: &str = "cursor-damaged-record";
const DAMAGED: &str = "bubbleId:cursor-damaged-record:b2";

fn store(work: &Path) -> PathBuf {
    let root = work.join("Cursor").join("User").join("globalStorage");
    fs::create_dir_all(&root).expect("the Cursor storage directory is created");
    let database =
        rusqlite::Connection::open(root.join("state.vscdb")).expect("the vendor store is created");
    database
        .execute_batch(&format!(
            "create table cursorDiskKV (key text unique on conflict replace, value blob);
             insert into cursorDiskKV values
               ('composerData:{CHAT}', '{{\"composerId\":\"{CHAT}\",\"createdAt\":1785600000000,\"fullConversationHeadersOnly\":[{{\"bubbleId\":\"b1\"}},{{\"bubbleId\":\"b2\"}}]}}'),
               ('bubbleId:{CHAT}:b1', '{{\"type\":1,\"text\":\"the readable question\"}}'),
               ('{DAMAGED}', '{{\"type\":2,\"text\":\"cut off');"
        ))
        .expect("the chat records load");
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
fn a_damaged_chat_record_is_named_and_the_rest_is_adopted() {
    let work = evidence::directory();
    let root = store(&work);
    let lake = work.join("lake");
    let output = evidence::run(
        &work,
        "adopt-damaged-record",
        &[
            "--data-dir",
            lake.to_str().expect("UTF-8 Lake path"),
            "adopt",
            "--source",
            "cursor",
            "--root",
            root.to_str().expect("UTF-8 native root"),
            "--json",
        ],
        &[],
    );
    let said = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "adopt failed: {said}");
    assert!(
        said.contains(&format!("chat record {DAMAGED}")) && said.contains("is not JSON"),
        "the damaged record was not named: {said}"
    );
    let questions: Vec<String> = adopted(&lake)
        .iter()
        .filter(|event| event["session_id"] == CHAT && event["event_type"] == "user")
        .filter_map(|event| event["text"].as_str().map(str::to_string))
        .collect();
    assert_eq!(questions, vec!["the readable question".to_string()]);
}
