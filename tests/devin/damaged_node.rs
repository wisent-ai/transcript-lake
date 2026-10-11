//! Real CLI journey: a Devin store with one message node whose chat message
//! is not JSON is adopted with every readable message, and the damaged node
//! is named on stderr with its session - it used to lose its text without a
//! word. The store is the observed-format registry's Devin fixture (beside
//! this file) plus that one damaged node, placed where the adapter discovers
//! it (`$XDG_DATA_HOME/devin/cli`); it is the vendor's store the adapter
//! reads, not product data. The journey is retained by `evidence`.
#[path = "../harnesses/evidence.rs"]
mod evidence;

use std::fs;
use std::path::{Path, PathBuf};

const FIXTURE: &str = include_str!("registry.sql");

/// A node on the parent session whose chat message was cut off mid-record.
const DAMAGED_NODE: &str = "insert into message_nodes \
     (session_id, node_id, parent_node_id, chat_message, created_at, metadata) values \
     ('registry-devin-sub', 77, 10, '{\"message_id\":\"cut\",\"role\":\"assistant\",\"content\":\"cut off', \
      1785600120, null);";

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
        .execute_batch(DAMAGED_NODE)
        .expect("the damaged node loads");
    (data, root)
}

#[test]
fn a_damaged_node_is_named_and_the_session_is_adopted() {
    let work = evidence::directory();
    let (data, root) = store(&work);
    let lake = work.join("lake");
    let output = evidence::run(
        &work,
        "adopt-damaged-node",
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
        &[("XDG_DATA_HOME", data.as_path())],
    );
    let said = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "adopt failed: {said}");
    assert!(
        said.contains("the message of node 77 of session registry-devin-sub")
            && said.contains("is not JSON"),
        "the damaged node was not named: {said}"
    );
}
