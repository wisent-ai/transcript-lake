//! Real CLI refusal and recovery against an isolated native Cursor SQLite store.
//! Malformed stored JSON must not publish a shortened conversation.
#[path = "../harnesses/evidence.rs"]
mod evidence;

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

const CHAT: &str = "cursor-damaged-record";
const DAMAGED: &str = "bubbleId:cursor-damaged-record:b2";

fn store(work: &Path) -> PathBuf {
    let root = work
        .join(".config")
        .join("Cursor")
        .join("User")
        .join("globalStorage");
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
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => panic!("cannot read {}: {error}", directory.display()),
        };
        for entry in entries {
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
fn a_damaged_chat_record_refuses_and_recovers_after_repair() {
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
        &[("HOME", work.as_path())],
    );
    let said = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "damaged record was accepted: {said}"
    );
    assert!(
        said.contains(&format!("chat record {DAMAGED}")) && said.contains("is not JSON"),
        "the damaged record was not named: {said}"
    );
    assert!(
        said.contains(&root.join("state.vscdb").display().to_string()),
        "{said}"
    );
    assert!(
        adopted(&lake).is_empty(),
        "refusal published an incomplete conversation"
    );
    let database = rusqlite::Connection::open(root.join("state.vscdb")).expect("open owned store");
    database.execute(
        "UPDATE cursorDiskKV SET value = (SELECT value FROM cursorDiskKV WHERE key = ?1) WHERE key = ?2",
        rusqlite::params![format!("bubbleId:{CHAT}:b1"), DAMAGED],
    ).expect("repair damaged record using the owned store's readable native message");
    let recovered = evidence::run(
        &work,
        "adopt-repaired-record",
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
        &[("HOME", work.as_path())],
    );
    assert!(
        recovered.status.success(),
        "{}",
        String::from_utf8_lossy(&recovered.stderr)
    );
    let expected: Value = serde_json::from_str(
        &database
            .query_row(
                "SELECT value FROM cursorDiskKV WHERE key = ?1",
                [DAMAGED],
                |row| row.get::<_, String>("value"),
            )
            .expect("read repaired native record"),
    )
    .expect("repaired native JSON");
    let committed = adopted(&lake);
    let messages: Vec<_> = committed
        .iter()
        .filter(|event| event["session_id"] == CHAT)
        .map(|event| event["text"].clone())
        .collect();
    let mut statement = database
        .prepare("SELECT key FROM cursorDiskKV WHERE key LIKE ?1")
        .expect("read native bubble identities");
    let expected_messages: Vec<_> = statement
        .query_map([format!("bubbleId:{CHAT}:%")], |_| {
            Ok(expected["text"].clone())
        })
        .expect("enumerate native bubbles")
        .collect::<Result<_, _>>()
        .expect("read every native bubble");
    assert_eq!(
        messages, expected_messages,
        "recovery lost or duplicated a native message"
    );
    let replay = evidence::run(
        &work,
        "replay-repaired-record",
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
        &[("HOME", work.as_path())],
    );
    assert!(
        replay.status.success(),
        "{}",
        String::from_utf8_lossy(&replay.stderr)
    );
    assert_eq!(
        adopted(&lake),
        committed,
        "replay changed persisted occurrences"
    );
}
