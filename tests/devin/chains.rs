//! Devin adoption rebuilds each session's conversation chains instead of
//! flattening the node table: the live chain reads a repeated message once,
//! a declared subagent's chain and a rewound branch keep their own identity
//! under the parent session, a branch that shares its prefix with the live
//! chain emits only what the live chain did not cover, and a hidden session
//! is not read.
//!
//! The store is a real `sessions.db` written with the product's own SQLite
//! from the observed-format registry's Devin fixture (the registry the
//! adapter's module documentation names; retained beside this file verbatim)
//! plus one rewound branch rooted on the live chain, placed where the
//! adapter discovers it (`$XDG_DATA_HOME/devin/cli`). It is the vendor's
//! store the adapter reads, not product data. Node ids in that store are
//! per session, as in Devin's: the hidden session's only node carries the
//! same id as the parent session's rewound root, and the adapter reads each
//! session's nodes by session, so the branch asserted below is the parent's
//! and the hidden session stays unread. The real `transcript-lake adopt`
//! runs over it and the persisted partitions are read back; the journey,
//! its inputs and outputs are retained under the package's build directory
//! by `evidence`.

#[path = "../harnesses/evidence.rs"]
mod evidence;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

const FIXTURE: &str = include_str!("registry.sql");

/// A branch a rewind left on the registry's parent session: rooted on the
/// live chain's user turn, so its prefix is already covered.
const REWOUND_BRANCH: &str = "insert into message_nodes \
     (session_id, node_id, parent_node_id, chat_message, created_at, metadata) values \
     ('registry-devin-sub', 18, 10, \
      '{\"message_id\":\"rw2-a\",\"role\":\"assistant\",\"content\":\"a draft the rewind discarded\"}', \
      1785600110, null);";

const PARENT: &str = "registry-devin-sub";
const SESSION: &str = "registry-devin";
const AGENT: &str = "ag-worker";

fn store(work: &Path) -> (PathBuf, PathBuf) {
    let data = work.join("data");
    let root = data.join("devin").join("cli");
    fs::create_dir_all(&root).expect("the native root is created");
    let database =
        rusqlite::Connection::open(root.join("sessions.db")).expect("the vendor store is created");
    database
        .execute_batch(FIXTURE)
        .expect("the registry fixture loads");
    database
        .execute_batch(REWOUND_BRANCH)
        .expect("the rewound branch loads");
    (data, root)
}

fn partitions(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut found = BTreeMap::new();
    for entry in fs::read_dir(root).expect("read canonical partition directory") {
        let path = entry.expect("read partition entry").path();
        if path.is_dir() {
            found.extend(partitions(&path));
        } else if path.extension().is_some_and(|ext| ext == "ndjson") {
            found.insert(
                path.clone(),
                fs::read(&path).expect("read persisted partition"),
            );
        }
    }
    found
}

fn adopt(work: &Path, data: &Path, root: &Path) -> Vec<Value> {
    let lake = work.join("lake");
    let formats = evidence::no_secret_classes(work);
    let output = evidence::run(
        work,
        "adopt",
        &[
            "--data-dir",
            lake.to_str().expect("UTF-8 Lake path"),
            "adopt",
            "--source",
            "devin",
            "--root",
            root.to_str().expect("UTF-8 native root"),
            "--json",
        ],
        &[
            ("XDG_DATA_HOME", data),
            ("TRANSCRIPT_LAKE_SECRET_FORMATS", formats.as_path()),
        ],
    );
    assert!(
        output.status.success(),
        "adopt: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    partitions(&lake.join("events"))
        .values()
        .flat_map(|bytes| {
            std::str::from_utf8(bytes)
                .expect("UTF-8 canonical partition")
                .lines()
                .map(|line| serde_json::from_str::<Value>(line).expect("canonical event"))
                .collect::<Vec<_>>()
        })
        .collect()
}

fn texts<'a>(events: &'a [Value], session: &str, kind: &str) -> Vec<&'a str> {
    events
        .iter()
        .filter(|event| event["session_id"] == session && event["event_type"] == kind)
        .filter_map(|event| event["text"].as_str())
        .collect()
}

#[test]
fn chains_are_rebuilt_from_the_node_graph() {
    let work = evidence::directory();
    let (data, root) = store(&work);
    let events = adopt(&work, &data, &root);

    let user = texts(&events, SESSION, "user");
    assert_eq!(
        user,
        vec!["the retry loop drops the last attempt"],
        "the live chain reads the compacted replay once: {user:?}"
    );
    assert!(
        events.iter().any(|event| {
            event["session_id"] == SESSION
                && event["event_type"] == "meta"
                && event["extra"]["kind"] == "system_prompt"
                && event["text"] == "DEJA-REGISTRY-INJECTED-CONTEXT"
        }),
        "injected hook context on the live chain is kept"
    );
    assert!(
        events
            .iter()
            .all(|event| event["text"] != "must not be read"),
        "a hidden session is not read"
    );
    let stamps: Vec<&str> = events
        .iter()
        .filter(|event| event["session_id"] == SESSION)
        .filter_map(|event| event["ts"].as_str())
        .collect();
    let mut ordered = stamps.clone();
    ordered.sort_unstable();
    assert_eq!(
        stamps, ordered,
        "the live chain is persisted root first, in conversation order"
    );
    assert_eq!(
        stamps.first().copied(),
        events
            .iter()
            .find(|event| event["session_id"] == SESSION && event["event_type"] == "user")
            .and_then(|event| event["ts"].as_str()),
        "the user turn opens the live chain"
    );

    let parent_user = texts(&events, PARENT, "user");
    assert_eq!(
        parent_user,
        vec!["run the worker"],
        "the parent's live chain holds its own turn alone: {parent_user:?}"
    );
    assert!(
        events
            .iter()
            .filter(|event| event["session_id"] == PARENT)
            .all(|event| event["extra"].get("parent_session").is_none()),
        "the live chain carries no parent"
    );

    let subagent = format!("{PARENT}:{AGENT}");
    assert_eq!(
        texts(&events, &subagent, "user"),
        vec!["echo registry-worker"],
        "the declared subagent's chain keeps its own identity"
    );
    let worker = events
        .iter()
        .find(|event| event["session_id"] == subagent)
        .expect("a subagent event");
    assert_eq!(worker["extra"]["parent_session"], PARENT);
    assert_eq!(worker["extra"]["chain"], AGENT);

    let branch = format!("{PARENT}:branch-16");
    assert_eq!(
        texts(&events, &branch, "user"),
        vec!["earlier draft of the ask"],
        "a rewound chain no head names is a branch under its root"
    );

    let rewound = format!("{PARENT}:branch-18");
    let rewound_events: Vec<&Value> = events
        .iter()
        .filter(|event| event["session_id"] == rewound)
        .collect();
    assert!(
        rewound_events
            .iter()
            .all(|event| event["text"] != "run the worker"),
        "a branch sharing the live chain's prefix does not emit that prefix again: \
         {rewound_events:?}"
    );
    assert!(
        rewound_events
            .iter()
            .any(|event| event["text"] == "a draft the rewind discarded"),
        "the branch's own content is kept: {rewound_events:?}"
    );
}
