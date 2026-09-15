//! Replaced source journals must not hide older operator instructions.
//! Every journey drives the real CLI and inspects both canonical and Oko output.
use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Read;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

mod interruption;
mod refusals;

const SESSION: &str = "01a08878-0000-7000-8000-000000000002";

struct Journey {
    root: PathBuf,
    home: PathBuf,
    source: PathBuf,
    journal: PathBuf,
    data: PathBuf,
    binary: PathBuf,
}

fn digest(path: &Path) -> String {
    let mut file = File::open(path).unwrap();
    let mut hash = Sha256::new();
    let mut buffer = [0; 8192];
    loop {
        let count = file.read(&mut buffer).unwrap();
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    format!("{:x}", hash.finalize())
}

fn message(id: &str, timestamp: &str, words: &str) -> Value {
    json!({"type":"message", "id":id, "timestamp":timestamp,
        "message":{"role":"user", "content":[{"type":"text", "text":words}]}})
}

impl Journey {
    fn new() -> Self {
        let root = Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join("source-rewrite")
            .join(uuid::Uuid::new_v4().to_string());
        let home = root.join("home");
        let source = home.join(".omp/agent/sessions/-Users-operator-work");
        fs::create_dir_all(&source).unwrap();
        let journal = source.join(format!("2026-09-12T06-00-00-000Z_{SESSION}.jsonl"));
        let data = root.join("lake");
        let binary = std::env::var_os("TRANSCRIPT_LAKE_BIN")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_transcript-lake")));
        for (name, args) in [
            ("revision.txt", vec!["rev-parse", "HEAD"]),
            ("changes.patch", vec!["diff", "HEAD"]),
        ] {
            let result = Command::new("git")
                .args(args)
                .current_dir(env!("CARGO_MANIFEST_DIR"))
                .output()
                .unwrap();
            assert!(result.status.success());
            fs::write(root.join(name), result.stdout).unwrap();
        }
        fs::write(root.join("binary.sha256"), digest(&binary)).unwrap();
        println!("Source rewrite evidence: {}", root.display());
        Self {
            root,
            home,
            source,
            journal,
            data,
            binary,
        }
    }

    fn replace(&self, messages: &[Value]) {
        let mut text = serde_json::to_string(&json!({"type":"session", "id":SESSION,
            "cwd":"/Users/operator/work", "version":"0.1.0"}))
        .unwrap();
        text.push('\n');
        for message in messages {
            text.push_str(&serde_json::to_string(message).unwrap());
            text.push('\n');
        }
        let staged = self.source.join("journal-replacement");
        fs::write(&staged, text).unwrap();
        fs::rename(staged, &self.journal).unwrap();
    }

    fn adopt(&self, label: &str) {
        let result = self.run_adopt(label);
        assert!(
            result.status.success(),
            "{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }

    fn run_adopt(&self, label: &str) -> Output {
        let args = [
            "--data-dir",
            self.data.to_str().unwrap(),
            "adopt",
            "--source",
            "omp",
            "--root",
            self.source.to_str().unwrap(),
            "--json",
        ];
        let result = Command::new(&self.binary)
            .args(args)
            .env("HOME", &self.home)
            .output()
            .unwrap();
        let receipt = json!({"binary":self.binary, "arguments":args, "exit_code":result.status.code(),
            "stdout":String::from_utf8_lossy(&result.stdout),
            "stderr":String::from_utf8_lossy(&result.stderr),
            "source_sha256":digest(&self.journal),
            "cursor":fs::read_to_string(self.data.join("cursors.json")).ok()});
        fs::write(
            self.root.join(format!("{label}.json")),
            serde_json::to_vec_pretty(&receipt).unwrap(),
        )
        .unwrap();
        result
    }

    fn assert_turns(&self, expected: &[&str]) {
        let expected: BTreeMap<String, usize> =
            expected.iter().map(|text| (text.to_string(), 1)).collect();
        let mut canonical = BTreeMap::new();
        let events = self.data.join("events/runtime=omp");
        for date in fs::read_dir(events).unwrap() {
            for file in fs::read_dir(date.unwrap().path()).unwrap() {
                collect_turns(&file.unwrap().path(), "event_type", &mut canonical);
            }
        }
        let mut projected = BTreeMap::new();
        for file in fs::read_dir(self.data.join("exports/oko/runtime=omp")).unwrap() {
            collect_turns(&file.unwrap().path(), "role", &mut projected);
        }
        fs::write(
            self.root.join("observed-turns.json"),
            serde_json::to_vec_pretty(
                &json!({"canonical":canonical, "projected":projected, "expected":expected}),
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(
            canonical, expected,
            "canonical history lost or duplicated a turn"
        );
        assert_eq!(
            projected, expected,
            "Oko cannot read every operator turn exactly once"
        );
    }
}

fn collect_turns(file: &Path, role: &str, result: &mut BTreeMap<String, usize>) {
    for line in fs::read_to_string(file).unwrap().lines() {
        let row: Value = serde_json::from_str(line).unwrap();
        if row[role] == "user" {
            *result
                .entry(row["text"].as_str().unwrap().to_string())
                .or_default() += 1;
        }
    }
}

#[test]
fn growing_replacement_recovers_earlier_instructions_without_duplicates() {
    let journey = Journey::new();
    let kept = message(
        "kept",
        "2026-09-12T06:00:02.000Z",
        "Keep the existing work.",
    );
    journey.replace(std::slice::from_ref(&kept));
    journey.adopt("initial");
    let earlier = "Recover the earlier instruction, including the explanation that precedes the old byte cursor. ".repeat(8);
    let late = "Finish the newly assigned work too.";
    journey.replace(&[
        message("earlier", "2026-09-12T06:00:01.000Z", &earlier),
        kept,
        message("latest", "2026-09-12T06:00:03.000Z", late),
    ]);
    journey.adopt("replaced");
    journey.assert_turns(&[&earlier, "Keep the existing work.", late]);
    journey.adopt("repeated");
    journey.assert_turns(&[&earlier, "Keep the existing work.", late]);
}

#[test]
fn replacement_with_preserved_size_and_timestamp_is_not_unchanged() {
    let journey = Journey::new();
    journey.replace(&[message(
        "same",
        "2026-09-12T06:00:01.000Z",
        "initial request",
    )]);
    journey.adopt("initial");
    let before = fs::metadata(&journey.journal).unwrap();
    journey.replace(&[message(
        "same",
        "2026-09-12T06:00:01.000Z",
        "updated request",
    )]);
    File::options()
        .write(true)
        .open(&journey.journal)
        .unwrap()
        .set_modified(before.modified().unwrap())
        .unwrap();
    assert_eq!(fs::metadata(&journey.journal).unwrap().len(), before.len());
    journey.adopt("same-metadata-replacement");
    journey.assert_turns(&["initial request", "updated request"]);
    journey.adopt("repeated");
    journey.assert_turns(&["initial request", "updated request"]);
}

#[test]
fn legacy_cursor_recovers_missing_history_even_without_a_new_append() {
    let journey = Journey::new();
    let kept = message(
        "kept",
        "2026-09-12T06:00:02.000Z",
        "The already archived turn.",
    );
    journey.replace(std::slice::from_ref(&kept));
    journey.adopt("initial");
    journey.replace(&[
        message(
            "missed",
            "2026-09-12T06:00:01.000Z",
            "The instruction the old cursor missed.",
        ),
        kept,
    ]);
    // Historical input: the documented three-field cursor written by the
    // previous ingestor, at EOF despite an unarchived rewritten prefix.
    // The real CLI created all canonical and projection state above.
    let meta = fs::metadata(&journey.journal).unwrap();
    let cursor = json!({"mtimeMs":meta.mtime() as f64 * 1000.0 + meta.mtime_nsec() as f64 / 1_000_000.0,
        "size":meta.len(), "offset":meta.len()});
    let store = json!({journey.journal.to_str().unwrap():cursor});
    fs::write(
        journey.data.join("cursors.json"),
        serde_json::to_vec_pretty(&store).unwrap(),
    )
    .unwrap();
    fs::write(
        journey.root.join("legacy-cursors.json"),
        serde_json::to_vec_pretty(&store).unwrap(),
    )
    .unwrap();
    journey.adopt("legacy-recovery");
    journey.assert_turns(&[
        "The already archived turn.",
        "The instruction the old cursor missed.",
    ]);
    journey.adopt("repeated");
    journey.assert_turns(&[
        "The already archived turn.",
        "The instruction the old cursor missed.",
    ]);
}
