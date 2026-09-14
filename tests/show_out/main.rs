//! `show --out` through the real binary, a real ingest and a real DuckDB.
//!
//! The operator asked for every message of a session in one file. Until this
//! option existed the only way was `show > file`, and a shell redirection
//! truncates the target before the command runs — a refusal or a dropped
//! connection left an empty file that looked like an answer. So the record is
//! written by the product, and these journeys drive that: a scratch HOME holds
//! one omp transcript, the product adopts it into a scratch Lake, and the file
//! it writes is read back and compared with what the same command prints.
//!
//! Nothing here touches the operator's own Lake or home: every path lives
//! under Cargo's target directory for this test binary.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const SESSION: &str = "01a0882d-0000-7000-8000-000000000001";

fn scratch(label: &str) -> PathBuf {
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("show-out-{label}"));
    if root.exists() {
        fs::remove_dir_all(&root).expect("clear the scratch root");
    }
    fs::create_dir_all(&root).expect("create the scratch root");
    root
}

/// One omp transcript where the adapter expects to find it, with the line
/// shapes its module documents: a session line and two messages.
fn seed_home(root: &Path) -> PathBuf {
    let home = root.join("home");
    let sessions = home
        .join(".omp")
        .join("agent")
        .join("sessions")
        .join("-Users-operator-work");
    fs::create_dir_all(&sessions).expect("create the omp session root");
    let transcript = sessions.join(format!("2026-09-11T20-00-00-000Z_{SESSION}.jsonl"));
    let lines = [
        format!(
            "{{\"type\":\"session\",\"id\":\"{SESSION}\",\"cwd\":\"/Users/operator/work\",\"version\":\"0.1.0\"}}"
        ),
        "{\"type\":\"message\",\"id\":\"m1\",\"timestamp\":\"2026-09-11T20:00:01.000Z\",\
         \"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"first question\"}]}}"
            .to_string(),
        "{\"type\":\"message\",\"id\":\"m2\",\"timestamp\":\"2026-09-11T20:00:02.000Z\",\
         \"message\":{\"role\":\"user\",\"content\":[{\"type\":\"text\",\"text\":\"second question\"}]}}"
            .to_string(),
    ];
    fs::write(&transcript, format!("{}\n", lines.join("\n"))).expect("write the transcript");
    home
}

fn lake(home: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_transcript-lake"))
        .args(args)
        .env("HOME", home)
        .output()
        .expect("run the product")
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).to_string()
}

/// The archive the reads below run against: adopted through the product's own
/// ingestion boundary, never by writing its files directly.
fn adopted(label: &str) -> (PathBuf, PathBuf) {
    let root = scratch(label);
    let home = seed_home(&root);
    let data_dir = root.join("lake");
    let source_root = home
        .join(".omp")
        .join("agent")
        .join("sessions")
        .join("-Users-operator-work");
    let adopt = lake(
        &home,
        &[
            "--data-dir",
            data_dir.to_str().expect("utf-8 data dir"),
            "adopt",
            "--source",
            "omp",
            "--root",
            source_root.to_str().expect("utf-8 source root"),
            "--json",
        ],
    );
    assert!(
        adopt.status.success(),
        "adopt failed: {}{}",
        text(&adopt.stdout),
        text(&adopt.stderr)
    );
    (root, home)
}

#[test]
fn the_written_record_is_the_record_the_command_prints() {
    let (root, home) = adopted("record");
    let data_dir = root.join("lake");
    let target = root.join("messages.txt");
    let printed = lake(
        &home,
        &[
            "--data-dir",
            data_dir.to_str().unwrap(),
            "show",
            SESSION,
            "--include",
            "user",
        ],
    );
    assert!(
        printed.status.success(),
        "show failed: {}",
        text(&printed.stderr)
    );
    let saved = lake(
        &home,
        &[
            "--data-dir",
            data_dir.to_str().unwrap(),
            "show",
            SESSION,
            "--include",
            "user",
            "--out",
            target.to_str().unwrap(),
        ],
    );
    assert!(
        saved.status.success(),
        "show --out failed: {}",
        text(&saved.stderr)
    );
    let body = fs::read_to_string(&target).expect("read the written record");
    assert_eq!(
        body,
        text(&printed.stdout),
        "the file must carry exactly what the terminal shows"
    );
    assert!(
        body.contains("first question") && body.contains("second question"),
        "every user message belongs in the file: {body}"
    );
    assert!(
        body.contains("rendered 2 of 2 matching events"),
        "the footer travels with the record: {body}"
    );
    let receipt = text(&saved.stdout);
    assert!(
        receipt.contains(target.to_str().unwrap())
            && receipt.contains(&format!("{} bytes", body.len()))
            && receipt.contains("2 of 2 matching events"),
        "the command says what it wrote: {receipt}"
    );
    assert!(
        !root.join("messages.txt.partial").exists(),
        "the sibling partial file is renamed, not left behind"
    );
}

#[test]
fn a_destination_that_cannot_hold_a_record_is_refused_before_the_read() {
    let (root, home) = adopted("refusals");
    let data_dir = root.join("lake");
    let missing = root.join("absent").join("messages.txt");
    let cases: [(String, &str); 4] = [
        // An option with nothing after it is the parser's own refusal; a value
        // made of spaces reaches the destination check, and both must say so.
        (String::new(), "--out requires a value"),
        ("   ".to_string(), "--out needs a file path"),
        (
            root.to_str().unwrap().to_string(),
            "is a directory: name the file to write",
        ),
        (missing.to_str().unwrap().to_string(), "does not exist"),
    ];
    for (index, (value, expected)) in cases.iter().enumerate() {
        let refused = lake(
            &home,
            &[
                "--data-dir",
                data_dir.to_str().unwrap(),
                "show",
                SESSION,
                "--out",
                value.as_str(),
            ],
        );
        assert!(
            !refused.status.success(),
            "case {index} must be refused: {}",
            text(&refused.stdout)
        );
        let said = format!("{}{}", text(&refused.stdout), text(&refused.stderr));
        assert!(said.contains(expected), "case {index} must say why: {said}");
    }
    assert!(
        !root.join("messages.txt").exists(),
        "a refused destination leaves no file behind"
    );
}
