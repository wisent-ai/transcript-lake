//! Real native-store expectations: runtime, root, session_id, masked user_text.
//! Vendor stores remain read-only; writes stay in the retained build directory.
mod evidence;
use serde::Deserialize;
use serde_json::Value;
use std::{collections::{BTreeMap, BTreeSet}, fs, path::{Path, PathBuf}};

#[derive(Deserialize)]
struct Expected {
    runtime: String,
    root: PathBuf,
    session_id: String,
    user_text: String,
}

fn partitions(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    let mut found = BTreeMap::new();
    for entry in fs::read_dir(root).expect("read canonical partition directory") {
        let path = entry.expect("read partition entry").path();
        if path.is_dir() {
            found.extend(partitions(&path));
        } else if path.extension().is_some_and(|ext| ext == "ndjson") {
            found.insert(path.clone(), fs::read(&path).expect("read persisted partition"));
        }
    }
    found
}

fn exercise(work: &Path, case: &Expected) {
    let lake = work.join("lake");
    let lake_path = lake.to_str().expect("UTF-8 Lake path");
    let source = case.root.to_str().expect("UTF-8 native source path");
    let args = ["--data-dir", lake_path, "adopt", "--source", &case.runtime, "--root", source, "--json"];
    let first = evidence::run(work, "adopt", &args);
    assert!(first.status.success(), "{}: {}", case.runtime, String::from_utf8_lossy(&first.stderr));
    let before = partitions(&lake.join("events"));
    let persisted: Vec<Value> = before.values().flat_map(|bytes| {
        std::str::from_utf8(bytes).expect("UTF-8 canonical partition").lines()
            .map(|line| serde_json::from_str::<Value>(line).expect("canonical event")).collect::<Vec<_>>()
    }).collect();
    assert!(persisted.iter().any(|event| {
        event["runtime"] == case.runtime && event["session_id"] == case.session_id
            && event["event_type"] == "user" && event["text"] == case.user_text
    }), "{} did not preserve the independently expected user message", case.runtime);
    let repeat = evidence::run(work, "adopt-repeat", &args);
    assert!(repeat.status.success(), "repeat {}: {}", case.runtime, String::from_utf8_lossy(&repeat.stderr));
    assert_eq!(partitions(&lake.join("events")), before, "unchanged native source must not duplicate events");
    let refused_lake = work.join("refused");
    let refused = evidence::run(work, "refused", &["--data-dir",
        refused_lake.to_str().expect("UTF-8 refusal destination"), "adopt", "--source",
        &case.runtime, "--root", work.to_str().expect("UTF-8 unowned source"), "--json"]);
    assert!(!refused.status.success(), "an undiscovered source root must be refused");
    assert!(String::from_utf8_lossy(&refused.stderr).contains("not a discovered"),
        "unexpected refusal: {}", String::from_utf8_lossy(&refused.stderr));
    assert!(!refused_lake.join("events").exists(), "refused adoption must not write events");
    assert!(!refused_lake.join("sources.json").exists(), "refused adoption must not select a source");
}

#[test]
#[ignore = "requires real native transcript stores and independently recorded expected messages"]
fn native_stores_preserve_messages_and_replay_without_duplicates() {
    let manifest = std::env::var("LAKE_TEST_EXPECTATIONS").expect("set LAKE_TEST_EXPECTATIONS");
    let minimum: std::num::NonZeroUsize = std::env::var("LAKE_TEST_MIN_HARNESSES")
        .expect("set LAKE_TEST_MIN_HARNESSES to the acceptance contract's required coverage")
        .parse().expect("LAKE_TEST_MIN_HARNESSES must be a positive integer");
    let body = fs::read(&manifest).expect("read actual-store expectations");
    let cases: Vec<Expected> = serde_json::from_slice(&body).expect("decode actual-store expectations");
    let distinct: BTreeSet<_> = cases.iter().map(|case| case.runtime.as_str()).collect();
    assert!(distinct.len() >= minimum.get(), "native stores do not meet required coverage {minimum}");
    let evidence = evidence::directory();
    fs::write(evidence.join("required-harnesses"), minimum.to_string()).expect("retain coverage contract");
    fs::write(evidence.join("expectations.json"), body).expect("retain exact acceptance inputs");
    for case in cases {
        assert!(!case.session_id.is_empty() && !case.user_text.is_empty(), "supply a known real message");
        let work = evidence.join(uuid::Uuid::new_v4().to_string());
        fs::create_dir(&work).expect("create isolated adoption workspace");
        exercise(&work, &case);
    }
}
