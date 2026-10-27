//! A stream that cannot catch up is a failed stream, in its own state and in
//! `status`, until a catch-up completes.
//!
//! The real `transcript-lake stream` runs in a HOME of its own with a Lake of
//! its own (`LAKE_DATA`). With no session root in that HOME it exits
//! non-zero before any catch-up, its state says `failed`, and `status` exits
//! non-zero printing `stream FAILED`. With one empty OMP session root and no
//! masking declaration, it exits non-zero naming the setting, its state says
//! `failed` with that error, and `status` names it; started again the same
//! way, as a supervisor restarting it would, `status` still names the
//! failure. Started with a declaration, it catches up, says so on its log
//! and its state carries no failure; stopped with SIGTERM it exits zero, and
//! `status` exits zero.

#[path = "../harnesses/evidence.rs"]
mod evidence;

use std::fs;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Output, Stdio};

use serde_json::Value;

const FORMATS: &str = "TRANSCRIPT_LAKE_SECRET_FORMATS";

fn state(lake: &Path) -> Value {
    let text = fs::read_to_string(lake.join("stream-status.json")).expect("read the stream state");
    serde_json::from_str(&text).expect("the stream state is JSON")
}

/// A start with no declaration fails naming the setting, keeps `failed`, and
/// `status` names it; `name` labels the retained evidence.
fn fails(work: &Path, lake: &Path, environment: &[(&str, &Path)], name: &str) -> Output {
    let stream = evidence::run(
        work,
        &format!("stream-{name}"),
        &["stream", "--json"],
        environment,
    );
    let said = String::from_utf8_lossy(&stream.stderr);
    assert!(
        !stream.status.success(),
        "a stream with no declaration caught up: {said}"
    );
    assert!(
        said.contains(FORMATS),
        "the refusal does not name the setting: {said}"
    );
    let kept = state(lake);
    assert_eq!(kept["state"], "failed", "{kept}");
    let error = kept["error"].as_str();
    assert!(error.is_some_and(|error| error.contains(FORMATS)), "{kept}");
    let status = evidence::run(work, &format!("status-{name}"), &["status"], environment);
    let printed = String::from_utf8_lossy(&status.stdout);
    assert!(
        !status.status.success(),
        "status passed a failed stream: {printed}"
    );
    assert!(
        printed.contains("stream FAILED") && printed.contains(FORMATS),
        "{printed}"
    );
    status
}

#[test]
fn a_stream_that_cannot_catch_up_is_failed_until_one_does() {
    let work = evidence::directory();
    let home = work.join("home");
    let lake = work.join("lake");
    let environment = [("HOME", home.as_path()), ("LAKE_DATA", lake.as_path())];

    // A HOME with no session root of any harness: the run ends before its
    // catch-up, and is kept as failed all the same.
    fs::create_dir_all(&home).expect("lay out an empty HOME");
    let rootless = evidence::run(
        &work,
        "stream-rootless",
        &["stream", "--json"],
        &environment,
    );
    let said = String::from_utf8_lossy(&rootless.stderr);
    assert!(
        !rootless.status.success(),
        "a stream with no source root ran: {said}"
    );
    let kept = state(&lake);
    assert_eq!(kept["state"], "failed", "{kept}");
    let error = kept["error"].as_str();
    assert!(
        error.is_some_and(|error| error.contains("source roots")),
        "{kept}"
    );
    let mut failed = vec![rootless];
    assert_eq!(kept["failedStarts"], failed.len(), "{kept}");
    let status = evidence::run(&work, "status-rootless", &["status"], &environment);
    let printed = String::from_utf8_lossy(&status.stdout);
    assert!(
        !status.status.success() && printed.contains("stream FAILED"),
        "{printed}"
    );

    fs::create_dir_all(home.join(".omp/agent/sessions")).expect("lay out an OMP session root");
    failed.push(fails(&work, &lake, &environment, "undeclared"));
    let restarted = fails(&work, &lake, &environment, "restarted-undeclared");
    failed.push(restarted);
    // Every start since the last completed catch-up failed, and is counted.
    assert_eq!(state(&lake)["failedStarts"], failed.len(), "{}", state(&lake));
    let printed = match failed.last() {
        Some(last) => String::from_utf8_lossy(&last.stdout).into_owned(),
        None => panic!("no failed start was retained"),
    };
    let counted = format!("{} starts in a row ended failed", failed.len());
    assert!(
        printed.contains(&counted),
        "status does not count the repeated failed starts ({counted}): {printed}"
    );

    let declaration = evidence::no_secret_classes(&work);
    let mut child = Command::new(env!("CARGO_BIN_EXE_transcript-lake"))
        .args(["stream", "--json"])
        .env("HOME", &home)
        .env("LAKE_DATA", &lake)
        .env(FORMATS, &declaration)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start the real stream");
    let stdout = child.stdout.take().expect("the stream's log");
    let mut log = BufReader::new(stdout).lines();
    let caught_up = loop {
        let line = log
            .next()
            .expect("the stream ended before catching up")
            .expect("read the log");
        let entry: Value = serde_json::from_str(&line).expect("each log line is JSON");
        if entry["kind"] == "catch-up" {
            break entry;
        }
    };
    let kept = state(&lake);
    let failing = kept["lastFailure"].is_object() || kept["state"] == "failed";
    assert!(!failing, "caught up, still failing: {kept}");
    let stopped = Command::new("kill")
        .args(["-TERM", &child.id().to_string()])
        .status()
        .expect("send SIGTERM");
    assert!(stopped.success(), "kill failed: {stopped}");
    let ended = child.wait().expect("the stream ends after SIGTERM");
    assert!(
        ended.success(),
        "a requested stop did not exit cleanly: {ended}; {caught_up}"
    );

    let status = evidence::run(&work, "status-recovered", &["status"], &environment);
    let printed = String::from_utf8_lossy(&status.stdout);
    assert!(
        status.status.success(),
        "status failed a recovered stream: {printed}"
    );
    assert!(!printed.contains("FAILED"), "{printed}");
}
