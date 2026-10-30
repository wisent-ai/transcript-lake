//! Real CLI journey: Transcript Lake runs on a machine with no Oko. With no
//! `oko-cli` on PATH and `OKO_CLI` naming none, `adopt` takes a Claude Code
//! transcript into the Lake, `doctor` is healthy and reports oko-cli as an
//! optional dependency that is absent, `projection rebuild --target oko`
//! writes the Oko projection under `exports/oko` without running anything of
//! Oko, and only `projection refresh --target oko`, the one hand-off to Oko,
//! refuses, naming oko-cli and how to provide it. This is what the catalogue's
//! optional dependency of transcript-lake on oko/cli states. The journey, its
//! inputs and outputs are retained under the package's build directory by
//! `evidence`.
#[path = "../harnesses/evidence.rs"]
mod evidence;

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

const SESSION: &str = "lake-without-oko";
const ANSWER: &str = "the Lake keeps this without Oko";

fn store(work: &Path) -> (PathBuf, PathBuf) {
    let home = work.join("home");
    let root = home.join(".claude").join("projects");
    let project = root.join("-work-lake-test");
    fs::create_dir_all(&project).expect("the Claude project directory is created");
    let lines = [
        format!("{{\"type\":\"user\",\"sessionId\":\"{SESSION}\",\"cwd\":\"/work/lake-test\",\"timestamp\":\"2026-06-02T03:35:11.000Z\",\"message\":{{\"role\":\"user\",\"content\":\"keep this\"}}}}"),
        format!("{{\"type\":\"assistant\",\"sessionId\":\"{SESSION}\",\"timestamp\":\"2026-06-02T03:35:12.000Z\",\"message\":{{\"model\":\"claude-test\",\"content\":[{{\"type\":\"text\",\"text\":\"{ANSWER}\"}}]}}}}"),
    ];
    fs::write(
        project.join(format!("{SESSION}.jsonl")),
        lines.join("\n") + "\n",
    )
    .expect("the transcript is written");
    (home, root)
}

/// Every file below `directory`, recursively; an absent directory has none.
fn files(directory: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![directory.to_path_buf()];
    while let Some(next) = pending.pop() {
        let entries = match fs::read_dir(&next) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => panic!("cannot list {}: {error}", next.display()),
        };
        for entry in entries {
            let path = entry.expect("read an entry").path();
            if path.is_dir() {
                pending.push(path);
            } else {
                found.push(path);
            }
        }
    }
    found
}

#[test]
fn the_lake_adopts_reads_and_projects_without_oko_and_refuses_only_the_hand_off() {
    let work = evidence::directory();
    let (home, root) = store(&work);
    let lake = work.join("lake");
    let formats = evidence::no_secret_classes(&work);
    // A PATH holding no oko-cli, and an OKO_CLI that names none.
    let bare = work.join("bin");
    fs::create_dir_all(&bare).expect("the empty program directory is created");
    let path = format!("{}:/usr/bin:/bin", bare.display());
    let environment = [
        ("HOME", home.as_path()),
        ("TRANSCRIPT_LAKE_SECRET_FORMATS", formats.as_path()),
        ("PATH", Path::new(&path)),
        ("OKO_CLI", Path::new("")),
        ("LAKE_DATA", lake.as_path()),
    ];
    let lake_arg = lake.to_str().expect("UTF-8 Lake path");

    let adopted = evidence::run(
        &work,
        "adopt-without-oko",
        &[
            "--data-dir",
            lake_arg,
            "adopt",
            "--source",
            "claude",
            "--root",
            root.to_str().expect("UTF-8 native root"),
            "--json",
        ],
        &environment,
    );
    let said = String::from_utf8_lossy(&adopted.stderr);
    assert!(adopted.status.success(), "adopt failed without Oko: {said}");
    let kept = files(&lake.join("events"))
        .iter()
        .filter(|path| path.extension().is_some_and(|ext| ext == "ndjson"))
        .flat_map(|path| {
            fs::read_to_string(path)
                .expect("read a persisted partition")
                .lines()
                .map(|line| serde_json::from_str::<Value>(line).expect("canonical event"))
                .collect::<Vec<_>>()
        })
        .any(|event| event["session_id"] == SESSION && event["text"] == ANSWER);
    assert!(kept, "the transcript was not kept without Oko");

    let doctor = evidence::run(
        &work,
        "doctor-without-oko",
        &["--data-dir", lake_arg, "doctor", "--json"],
        &environment,
    );
    let report: Value =
        serde_json::from_slice(&doctor.stdout).expect("doctor --json answers a JSON report");
    assert!(
        doctor.status.success() && report["healthy"] == true,
        "doctor is not healthy without Oko: {report}"
    );
    let oko = report["checks"]
        .as_array()
        .expect("doctor lists its checks")
        .iter()
        .find(|check| check["name"] == "oko-cli")
        .unwrap_or_else(|| panic!("doctor does not report oko-cli: {report}"));
    assert_eq!(oko["status"], "warning", "{report}");
    assert!(
        oko["detail"]
            .as_str()
            .is_some_and(|detail| detail.contains("optional dependency not found")),
        "{report}"
    );

    let rebuilt = evidence::run(
        &work,
        "projection-rebuild-without-oko",
        &[
            "--data-dir",
            lake_arg,
            "projection",
            "rebuild",
            "--target",
            "oko",
        ],
        &environment,
    );
    assert!(
        rebuilt.status.success(),
        "the Oko projection was not written without Oko: {}",
        String::from_utf8_lossy(&rebuilt.stderr)
    );
    assert!(
        !files(&lake.join("exports").join("oko")).is_empty(),
        "projection rebuild wrote nothing under exports/oko"
    );

    let refreshed = evidence::run(
        &work,
        "projection-refresh-without-oko",
        &[
            "--data-dir",
            lake_arg,
            "projection",
            "refresh",
            "--target",
            "oko",
        ],
        &environment,
    );
    let said = String::from_utf8_lossy(&refreshed.stderr);
    assert!(
        !refreshed.status.success(),
        "the hand-off to Oko succeeded with no oko-cli: {said}"
    );
    assert!(
        said.contains("oko-cli is not on PATH; install Oko or set OKO_CLI"),
        "the refusal does not name oko-cli and how to provide it: {said}"
    );
}
