//! `transcript-lake scrub` through the real binary on a scratch Lake.
//!
//! A credential literal stored in an append-only partition is replaced by the
//! masker's marker with every line still parsing; a second run changes
//! nothing; a preview writes nothing; a literal present in more files than
//! `--max-files` is refused; and a rewrite that would break a JSON line leaves
//! the file untouched. Nothing here touches the operator's own Lake: every path
//! lives under Cargo's target directory for this test binary.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use sha2::{Digest, Sha256};

const SECRET: &str = "Zq7!mK2#pL9vR4x";

fn scratch(label: &str) -> PathBuf {
    let root = Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!("scrub-{label}"));
    if root.exists() {
        fs::remove_dir_all(&root).expect("clear the scratch root");
    }
    fs::create_dir_all(root.join("events/runtime=omp/2026-09-27")).expect("create a partition");
    root
}

fn partition(root: &Path, name: &str) -> PathBuf {
    root.join("events/runtime=omp/2026-09-27").join(name)
}

fn scrub(root: &Path, extra: &[&str], literals: &str) -> Output {
    let binary = std::env::var_os("TRANSCRIPT_LAKE_TEST_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_transcript-lake")));
    let mut child = Command::new(binary)
        .args(["--data-dir"])
        .arg(root)
        .args(["scrub", "--secret-file", "-"])
        .args(extra)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start transcript-lake");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(literals.as_bytes())
        .expect("write literals");
    child.wait_with_output().expect("run transcript-lake scrub")
}

fn marker() -> String {
    let digest = format!("{:x}", Sha256::digest(SECRET.as_bytes()));
    format!(
        "[masked:credential:{}:{}]",
        SECRET.chars().count(),
        &digest[..8]
    )
}

#[test]
fn a_stored_literal_is_masked_once_and_every_line_still_parses() {
    let root = scratch("apply");
    let file = partition(&root, "events.ndjson");
    let line = format!("{{\"text\":\"the password is {SECRET} today\"}}\n{{\"text\":\"clean\"}}\n");
    fs::write(&file, &line).expect("seed the partition");

    let preview = scrub(&root, &[], SECRET);
    assert!(preview.status.success(), "{preview:?}");
    assert_eq!(
        fs::read_to_string(&file).unwrap(),
        line,
        "a preview must not write"
    );
    assert!(
        !String::from_utf8_lossy(&preview.stdout).contains(SECRET),
        "the literal was printed"
    );

    let applied = scrub(&root, &["--apply"], SECRET);
    assert!(applied.status.success(), "{applied:?}");
    let rewritten = fs::read_to_string(&file).unwrap();
    assert!(!rewritten.contains(SECRET), "{rewritten}");
    assert!(rewritten.contains(&marker()), "{rewritten}");
    for text in rewritten.lines() {
        serde_json::from_str::<serde_json::Value>(text).expect("every line still parses");
    }
    assert!(
        !root.join("stream.lock").exists(),
        "the writer lease was not released"
    );

    let again = scrub(&root, &["--apply"], SECRET);
    assert!(again.status.success(), "{again:?}");
    assert_eq!(
        fs::read_to_string(&file).unwrap(),
        rewritten,
        "a second run changed the file"
    );
}

#[test]
fn a_literal_in_more_files_than_the_cap_is_refused_and_nothing_is_written() {
    let root = scratch("cap");
    let line = format!("{{\"text\":\"{SECRET}\"}}\n");
    for name in ["a.ndjson", "b.ndjson"] {
        fs::write(partition(&root, name), &line).expect("seed");
    }
    let refused = scrub(&root, &["--apply", "--max-files", "1"], SECRET);
    assert_eq!(refused.status.code(), Some(1), "{refused:?}");
    assert!(String::from_utf8_lossy(&refused.stdout).contains("REFUSED over cap"));
    assert_eq!(
        fs::read_to_string(partition(&root, "a.ndjson")).unwrap(),
        line
    );
}

#[test]
fn a_rewrite_that_would_not_parse_leaves_the_file_whole() {
    let root = scratch("parse");
    // The literal spans a JSON string boundary, so replacing it would break
    // the line: the file must be left exactly as it was.
    let file = partition(&root, "events.ndjson");
    let line = "{\"a\":\"x\",\"b\":\"secret-Val9\"}\n".to_string();
    fs::write(&file, &line).expect("seed");
    let refused = scrub(&root, &["--apply"], "x\",\"b\":\"secret-Val9");
    assert_eq!(refused.status.code(), Some(1), "{refused:?}");
    assert!(String::from_utf8_lossy(&refused.stdout).contains("would not parse"));
    assert_eq!(fs::read_to_string(&file).unwrap(), line);
}
