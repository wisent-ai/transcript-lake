//! Real CLI journey: a Droid session adopted through the real binary, whose
//! canonical rows are then put back in the shape an older adapter wrote them
//! (a provider notice recorded as an operator turn, no sender role), is
//! reconciled against its retained original. The preview names the
//! partition and changes nothing; the apply supersedes the stale rows without
//! duplicating any, leaves the stream cursor after them, and a second run finds
//! nothing to change. Once the original is gone the partition is unprovable:
//! reported, exit 1, and kept byte for byte. Inputs, outputs and the source
//! revision are retained under the package's build directory by `evidence`.
#[path = "../harnesses/evidence.rs"]
mod evidence;

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

const SESSION: &str = "reconcile-stale-projection";
const NOTICE: &str = "A provider notice the operator never typed";
const TURN: &str = "The prompt the operator typed";

fn store(home: &Path) -> PathBuf {
    let root = home.join(".factory").join("sessions");
    fs::create_dir_all(&root).expect("the Droid sessions directory is created");
    let transcript = root.join(format!("{SESSION}.jsonl"));
    fs::write(
        &transcript,
        format!(
            concat!(
                "{{\"type\":\"session_start\",\"id\":\"{session}\",\"title\":\"reconcile journey\"}}\n",
                "{{\"type\":\"message\",\"id\":\"notice\",\"timestamp\":\"2026-01-02T03:04:05.000Z\",\"message\":{{\"role\":\"user\",\"visibility\":\"llm_only\",\"content\":[{{\"type\":\"text\",\"text\":\"{notice}\"}}]}}}}\n",
                "{{\"type\":\"message\",\"id\":\"turn\",\"timestamp\":\"2026-01-02T03:04:06.000Z\",\"message\":{{\"role\":\"user\",\"content\":[{{\"type\":\"text\",\"text\":\"{turn}\"}}]}}}}\n",
            ),
            session = SESSION,
            notice = NOTICE,
            turn = TURN,
        ),
    )
    .expect("the native transcript is written");
    transcript
}

/// Every canonical partition file of the droid runtime.
fn partitions(lake: &Path) -> Vec<PathBuf> {
    let base = lake.join("events").join("runtime=droid");
    let mut files = Vec::new();
    for date in fs::read_dir(&base).expect("list droid date partitions") {
        let date = date.expect("read a date partition").path();
        for file in fs::read_dir(&date).expect("list a date partition") {
            let file = file.expect("read a partition entry").path();
            if file.extension().is_some_and(|ext| ext == "ndjson") {
                files.push(file);
            }
        }
    }
    files.sort();
    files
}

fn rows(lake: &Path) -> Vec<Value> {
    partitions(lake)
        .iter()
        .flat_map(|file| {
            fs::read_to_string(file)
                .expect("read a canonical partition")
                .lines()
                .map(|line| serde_json::from_str::<Value>(line).expect("canonical event"))
                .collect::<Vec<_>>()
        })
        .collect()
}

fn snapshot(lake: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    partitions(lake)
        .into_iter()
        .map(|file| {
            let bytes = fs::read(&file).expect("read a canonical partition");
            (file, bytes)
        })
        .collect()
}

/// Rewrite the session's two message rows as the adapter wrote them before
/// it read the sender: the notice typed as a user turn, neither with a role.
/// Returns the texts of the rows made stale.
fn make_stale(lake: &Path) -> Vec<Value> {
    let mut stale = Vec::new();
    for file in partitions(lake) {
        let text = fs::read_to_string(&file).expect("read a canonical partition");
        let mut lines = Vec::new();
        for line in text.lines() {
            let mut row: Value = serde_json::from_str(line).expect("canonical event");
            if row["text"] == NOTICE || row["text"] == TURN {
                let extra = row["extra"].as_object_mut().expect("extra object");
                extra.retain(|key, _| key == "source_stem_hash");
                row["event_type"] = Value::from("user");
                stale.push(row["text"].clone());
            }
            lines.push(serde_json::to_string(&row).expect("encode a stale row"));
        }
        fs::write(&file, lines.join("\n") + "\n").expect("write the stale partition");
    }
    stale
}

fn report(output: &std::process::Output) -> Value {
    serde_json::from_slice(&output.stdout).expect("reconcile prints one JSON document")
}

fn names_session(entry: &Value) -> bool {
    entry["sessions"]
        .as_array()
        .is_some_and(|sessions| sessions.iter().any(|session| session == SESSION))
}

#[test]
fn stale_rows_are_superseded_from_their_original_and_a_missing_original_is_unprovable() {
    let work = evidence::directory();
    let home = work.join("home");
    let transcript = store(&home);
    let lake = work.join("lake");
    let lake_arg = lake.to_str().expect("UTF-8 Lake path");
    let environment: &[(&str, &Path)] = &[("HOME", home.as_path())];
    let root = home.join(".factory").join("sessions");
    let root_arg = root.to_str().expect("UTF-8 native root");
    let adopt = [
        "--data-dir",
        lake_arg,
        "adopt",
        "--source",
        "droid",
        "--root",
        root_arg,
        "--json",
    ];
    let adopted = evidence::run(&work, "adopt", &adopt, environment);
    assert!(
        adopted.status.success(),
        "adopt failed: {}",
        String::from_utf8_lossy(&adopted.stderr)
    );
    let current = rows(&lake);
    let stale = make_stale(&lake);
    assert!(!stale.is_empty(), "the journey made no row stale");
    let before = snapshot(&lake);

    let preview = evidence::run(
        &work,
        "reconcile-preview",
        &["--data-dir", lake_arg, "reconcile", "--source", "droid"],
        environment,
    );
    assert!(
        preview.status.success(),
        "preview failed: {}",
        String::from_utf8_lossy(&preview.stderr)
    );
    let previewed = report(&preview);
    assert_eq!(previewed["applied"], false);
    assert!(
        previewed["partitions"]["changed"]
            .as_array()
            .is_some_and(|changed| changed.iter().any(|partition| {
                names_session(partition)
                    && partition["rowsSuperseded"] == stale.len()
                    && partition["rowsReplayed"] == stale.len()
            })),
        "the stale partition was not reported: {previewed}"
    );
    assert_eq!(snapshot(&lake), before, "a preview changed the Lake");

    let applied = evidence::run(
        &work,
        "reconcile-apply",
        &[
            "--data-dir",
            lake_arg,
            "reconcile",
            "--source",
            "droid",
            "--apply",
        ],
        environment,
    );
    assert!(
        applied.status.success(),
        "apply failed: {}",
        String::from_utf8_lossy(&applied.stderr)
    );
    assert_eq!(report(&applied)["applied"], true);
    let reconciled = rows(&lake);
    assert_eq!(
        reconciled.len(),
        current.len(),
        "the apply changed how many rows the session holds"
    );
    let notices: Vec<&Value> = reconciled
        .iter()
        .filter(|row| row["text"] == NOTICE)
        .collect();
    assert!(
        matches!(notices.as_slice(), [notice] if notice["event_type"] == "meta"
            && notice["extra"]["visibility"] == "llm_only"),
        "the provider notice is not one meta row carrying its visibility: {notices:?}"
    );
    assert!(
        reconciled.iter().any(|row| row["text"] == TURN
            && row["event_type"] == "user"
            && row["extra"]["role"] == "user"),
        "the typed turn did not keep its sender"
    );

    let readopted = evidence::run(&work, "adopt-after-apply", &adopt, environment);
    assert!(
        readopted.status.success(),
        "adopt after apply failed: {}",
        String::from_utf8_lossy(&readopted.stderr)
    );
    assert_eq!(
        rows(&lake).len(),
        reconciled.len(),
        "the stream appended what the reconciled partition already held"
    );

    let again = evidence::run(
        &work,
        "reconcile-again",
        &["--data-dir", lake_arg, "reconcile", "--source", "droid"],
        environment,
    );
    assert!(again.status.success());
    assert_eq!(
        report(&again)["partitions"]["changed"],
        Value::Array(Vec::new()),
        "a reconciled Lake still differs from its original"
    );

    fs::remove_file(&transcript).expect("remove the original");
    let kept = snapshot(&lake);
    let orphaned = evidence::run(
        &work,
        "reconcile-unprovable",
        &[
            "--data-dir",
            lake_arg,
            "reconcile",
            "--source",
            "droid",
            "--apply",
        ],
        environment,
    );
    assert!(
        !orphaned.status.success(),
        "a partition without its original was reported as proved"
    );
    let unprovable = report(&orphaned)["partitions"]["unprovable"].clone();
    assert!(
        unprovable
            .as_array()
            .is_some_and(|entries| entries.iter().any(names_session)),
        "the partition without an original is not named unprovable: {unprovable}"
    );
    assert_eq!(
        snapshot(&lake),
        kept,
        "an unprovable partition was rewritten"
    );

    let lone = evidence::run(
        &work,
        "reconcile-reindex-alone",
        &[
            "--data-dir",
            lake_arg,
            "reconcile",
            "--source",
            "droid",
            "--reindex",
        ],
        environment,
    );
    assert!(!lone.status.success());
    assert!(String::from_utf8_lossy(&lone.stderr).contains("reconcile --reindex needs --apply"));
}
