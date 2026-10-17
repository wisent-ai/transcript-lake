//! Real CLI journey: the writer lease of a Lake root (`stream.lock`) is held
//! while its recorded owner runs, and broken once that owner is gone. The
//! owner is told by its host, process id and the start time `/bin/ps`
//! reports, so a record naming a running process with another start time is
//! a dead owner whose id was reused. An owner record that exists and cannot
//! be read refuses the write; one that is not a whole owner record is
//! abandoned with a word on stderr. `transcript-lake adopt` takes the lease;
//! the journey, its inputs and outputs are retained under the package's
//! build directory by `evidence`.
#[path = "../harnesses/evidence.rs"]
mod evidence;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::{json, Value};

const SESSION: &str = "lease-owner";
const ANSWER: &str = "the reply written once the lease is free";

/// What a program prints on stdout, trimmed; it must succeed.
fn printed(program: &str, args: &[&str]) -> String {
    let output = match Command::new(program).args(args).output() {
        Ok(output) => output,
        Err(error) => panic!("{program} could not run: {error}"),
    };
    assert!(
        output.status.success(),
        "{program} {args:?} failed: {output:?}"
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// This test process as a lease owner: the machine's name, its id and the
/// start time `/bin/ps` reports for it.
fn this_process() -> (String, u32, String) {
    let pid = std::process::id();
    let started = printed("/bin/ps", &["-o", "lstart=", "-p", &pid.to_string()]);
    (printed("hostname", &[]), pid, started)
}

/// A Claude store with one session for `adopt` to write.
fn store(work: &Path) -> (PathBuf, PathBuf) {
    let home = work.join("home");
    let root = home.join(".claude").join("projects");
    let project = root.join("-work-lake-test");
    fs::create_dir_all(&project).expect("the Claude project directory is created");
    let lines = [
        format!("{{\"type\":\"user\",\"sessionId\":\"{SESSION}\",\"cwd\":\"/work/lake-test\",\"timestamp\":\"2026-06-02T03:35:11.000Z\",\"message\":{{\"role\":\"user\",\"content\":\"take the lease\"}}}}"),
        format!("{{\"type\":\"assistant\",\"sessionId\":\"{SESSION}\",\"timestamp\":\"2026-06-02T03:35:12.000Z\",\"message\":{{\"model\":\"claude-test\",\"content\":[{{\"type\":\"text\",\"text\":\"{ANSWER}\"}}]}}}}"),
    ];
    fs::write(
        project.join(format!("{SESSION}.jsonl")),
        lines.join("\n") + "\n",
    )
    .expect("the transcript is written");
    (home, root)
}

/// Publish `record` as the owner of the Lake's writer lease.
fn hold(lake: &Path, record: &str) -> PathBuf {
    let lock = lake.join("stream.lock");
    fs::create_dir_all(&lock).expect("the lease directory is created");
    let owner = lock.join("owner.json");
    fs::write(&owner, record).expect("the owner record is written");
    owner
}

fn adopt(work: &Path, name: &str, lake: &Path, home: &Path, root: &Path) -> Output {
    let formats = evidence::no_secret_classes(work);
    evidence::run(
        work,
        name,
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
            ("HOME", home),
            ("TRANSCRIPT_LAKE_SECRET_FORMATS", formats.as_path()),
        ],
    )
}

fn adopted(lake: &Path) -> bool {
    let mut directories = vec![lake.join("events")];
    while let Some(directory) = directories.pop() {
        for entry in fs::read_dir(&directory).expect("read a partition directory") {
            let path = entry.expect("read a partition entry").path();
            if path.is_dir() {
                directories.push(path);
            } else if path.extension().is_some_and(|ext| ext == "ndjson") {
                let text = fs::read_to_string(&path).expect("read a persisted partition");
                let found = text.lines().any(|line| {
                    let event: Value = serde_json::from_str(line).expect("canonical event");
                    event["session_id"] == SESSION && event["text"] == ANSWER
                });
                if found {
                    return true;
                }
            }
        }
    }
    false
}

#[test]
fn a_running_owner_holds_the_lease_and_a_reused_id_does_not() {
    let work = evidence::directory();
    let (home, root) = store(&work);
    let (host, pid, started) = this_process();

    let lake = work.join("held");
    let record = json!({
        "host": host,
        "pid": pid,
        "started": started,
        "token": "held"
    })
    .to_string();
    let owner = hold(&lake, &record);
    let output = adopt(&work, "adopt-held", &lake, &home, &root);
    let said = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "adopt wrote under a live owner: {said}"
    );
    assert!(
        said.contains(&format!("state writer lock is held by {host} pid {pid}")),
        "the live owner was not named: {said}"
    );
    assert_eq!(
        fs::read_to_string(&owner).expect("the live owner's record is still there"),
        record,
        "the live owner's lease was touched"
    );

    let lake = work.join("reused");
    let record = json!({
        "host": host,
        "pid": pid,
        "started": "a start time this process never had",
        "token": "reused"
    })
    .to_string();
    hold(&lake, &record);
    let output = adopt(&work, "adopt-reused", &lake, &home, &root);
    let said = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "adopt did not break a dead owner's lease: {said}"
    );
    assert!(
        adopted(&lake),
        "the session was not adopted once the lease was broken"
    );
    assert!(
        !lake.join("stream.lock").exists(),
        "the lease adopt took was not released"
    );
}

#[test]
fn an_unreadable_owner_refuses_and_a_partial_one_is_abandoned() {
    let work = evidence::directory();
    let (home, root) = store(&work);

    let lake = work.join("unreadable");
    let owner = hold(&lake, "{}");
    let granted = fs::metadata(&owner)
        .expect("the owner record's permissions are read")
        .permissions();
    let locked = Command::new("chmod")
        .arg("a-r")
        .arg(&owner)
        .status()
        .expect("chmod runs");
    assert!(
        locked.success(),
        "chmod could not lock the owner record {}",
        owner.display()
    );
    let output = adopt(&work, "adopt-unreadable", &lake, &home, &root);
    // The retained journey directory stays readable.
    fs::set_permissions(&owner, granted).expect("the owner record gets its permissions back");
    let said = String::from_utf8_lossy(&output.stderr);
    assert!(
        !output.status.success(),
        "adopt wrote past an unreadable owner: {said}"
    );
    assert!(
        said.contains(&format!(
            "state writer lock owner {} cannot be read",
            owner.display()
        )),
        "the unreadable owner record was not named: {said}"
    );
    assert!(owner.exists(), "the unreadable owner's lease was broken");

    let lake = work.join("partial");
    let owner = hold(&lake, "{\"host\":");
    let output = adopt(&work, "adopt-partial", &lake, &home, &root);
    let said = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "adopt did not abandon a partial owner: {said}"
    );
    assert!(
        said.contains(&format!(
            "cursors: lease owner {} is not a whole owner record",
            owner.display()
        )),
        "the partial owner record was not said: {said}"
    );
    assert!(
        adopted(&lake),
        "the session was not adopted once the lease was abandoned"
    );
}
