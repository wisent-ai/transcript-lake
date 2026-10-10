//! Real native-store expectations: runtime, root, session_id, masked user_text.
//! Vendor stores remain read-only; writes stay in the retained build directory.
mod evidence;
use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
};

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
            found.insert(
                path.clone(),
                fs::read(&path).expect("read persisted partition"),
            );
        }
    }
    found
}

fn exercise(work: &Path, case: &Expected, environment: &[(&str, &Path)]) {
    let lake = work.join("lake");
    let lake_path = lake.to_str().expect("UTF-8 Lake path");
    let source = case.root.to_str().expect("UTF-8 native source path");
    let args = [
        "--data-dir",
        lake_path,
        "adopt",
        "--source",
        &case.runtime,
        "--root",
        source,
        "--json",
    ];
    let first = evidence::run(work, "adopt", &args, environment);
    assert!(
        first.status.success(),
        "{}: {}",
        case.runtime,
        String::from_utf8_lossy(&first.stderr)
    );
    let before = partitions(&lake.join("events"));
    let persisted: Vec<Value> = before
        .values()
        .flat_map(|bytes| {
            std::str::from_utf8(bytes)
                .expect("UTF-8 canonical partition")
                .lines()
                .map(|line| serde_json::from_str::<Value>(line).expect("canonical event"))
                .collect::<Vec<_>>()
        })
        .collect();
    assert!(
        persisted.iter().any(|event| {
            event["runtime"] == case.runtime
                && event["session_id"] == case.session_id
                && event["event_type"] == "user"
                && event["text"] == case.user_text
        }),
        "{} did not preserve the independently expected user message",
        case.runtime
    );
    let repeat = evidence::run(work, "adopt-repeat", &args, environment);
    assert!(
        repeat.status.success(),
        "repeat {}: {}",
        case.runtime,
        String::from_utf8_lossy(&repeat.stderr)
    );
    assert_eq!(
        partitions(&lake.join("events")),
        before,
        "unchanged native source must not duplicate events"
    );
    let refused_lake = work.join("refused");
    let refused = evidence::run(
        work,
        "refused",
        &[
            "--data-dir",
            refused_lake.to_str().expect("UTF-8 refusal destination"),
            "adopt",
            "--source",
            &case.runtime,
            "--root",
            work.to_str().expect("UTF-8 unowned source"),
            "--json",
        ],
        environment,
    );
    assert!(
        !refused.status.success(),
        "an undiscovered source root must be refused"
    );
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("not a discovered"),
        "unexpected refusal: {}",
        String::from_utf8_lossy(&refused.stderr)
    );
    assert!(
        !refused_lake.join("events").exists(),
        "refused adoption must not write events"
    );
    assert!(
        !refused_lake.join("sources.json").exists(),
        "refused adoption must not select a source"
    );
}

#[test]
#[ignore = "requires real native transcript stores and independently recorded expected messages"]
fn native_stores_preserve_messages_and_replay_without_duplicates() {
    let manifest = std::env::var("LAKE_TEST_EXPECTATIONS").expect("set LAKE_TEST_EXPECTATIONS");
    let minimum: std::num::NonZeroUsize = std::env::var("LAKE_TEST_MIN_HARNESSES")
        .expect("set LAKE_TEST_MIN_HARNESSES to the acceptance contract's required coverage")
        .parse()
        .expect("LAKE_TEST_MIN_HARNESSES must be a positive integer");
    let body = fs::read(&manifest).expect("read actual-store expectations");
    let cases: Vec<Expected> =
        serde_json::from_slice(&body).expect("decode actual-store expectations");
    let distinct: BTreeSet<_> = cases.iter().map(|case| case.runtime.as_str()).collect();
    assert!(
        distinct.len() >= minimum.get(),
        "native stores do not meet required coverage {minimum}"
    );
    let evidence = evidence::directory();
    fs::write(evidence.join("required-harnesses"), minimum.to_string())
        .expect("retain coverage contract");
    fs::write(evidence.join("expectations.json"), body).expect("retain exact acceptance inputs");
    for case in cases {
        assert!(
            !case.session_id.is_empty() && !case.user_text.is_empty(),
            "supply a known real message"
        );
        let work = evidence.join(uuid::Uuid::new_v4().to_string());
        fs::create_dir(&work).expect("create isolated adoption workspace");
        exercise(&work, &case, &[]);
    }
}

#[test]
#[ignore = "requires a real Devin store and an independent message expectation"]
fn devin_native_data_directory_is_discovered_and_adopted() {
    let data = PathBuf::from(
        std::env::var_os("LAKE_TEST_DEVIN_DATA_HOME")
            .expect("set LAKE_TEST_DEVIN_DATA_HOME to the real native data directory"),
    );
    assert!(data.is_absolute(), "native data directory must be absolute");
    let environment = [("XDG_DATA_HOME", data.as_path())];
    let manifest = std::env::var_os("LAKE_TEST_DEVIN_EXPECTATION")
        .expect("set LAKE_TEST_DEVIN_EXPECTATION to one independently recorded Devin expectation");
    let body = fs::read(manifest).expect("read native Devin expectation");
    let case: Expected = serde_json::from_slice(&body).expect("parse native Devin expectation");
    assert_eq!(case.runtime, "devin");
    assert_eq!(case.root, data.join("devin").join("cli"));
    assert!(
        !case.session_id.is_empty() && !case.user_text.is_empty(),
        "supply a known native user message"
    );
    assert!(
        case.root.join("sessions.db").is_file(),
        "real native database must already exist"
    );
    let work = evidence::directory();
    fs::write(work.join("expectation.json"), body).expect("retain native expectation");
    fs::write(
        work.join("XDG_DATA_HOME"),
        data.as_os_str().as_encoded_bytes(),
    )
    .expect("retain native environment");
    let source = evidence::run(&work, "sources", &["sources", "--json"], &environment);
    assert!(
        source.status.success(),
        "discovery failed: {}",
        String::from_utf8_lossy(&source.stderr)
    );
    let rows: Vec<Value> = serde_json::from_slice(&source.stdout).expect("native source rows");
    let devin = rows
        .iter()
        .find(|row| row["runtime"] == "devin")
        .expect("registered Devin runtime");
    assert!(
        devin["roots"]
            .as_array()
            .expect("discovered roots")
            .iter()
            .any(|root| root.as_str() == case.root.to_str()),
        "native XDG store was not discovered: {devin}"
    );
    let unavailable = work.join("unavailable-data-home");
    let absent = evidence::run(
        &work,
        "sources-unavailable",
        &["sources", "--json"],
        &[("XDG_DATA_HOME", unavailable.as_path())],
    );
    assert!(
        absent.status.success(),
        "missing native stores are not discovery failures"
    );
    let absent_rows: Vec<Value> =
        serde_json::from_slice(&absent.stdout).expect("source rows with absent store");
    let absent_devin = absent_rows
        .iter()
        .find(|row| row["runtime"] == "devin")
        .expect("registered Devin runtime");
    assert_eq!(
        absent_devin["available"], false,
        "Devin must not read another data directory: {absent_devin}"
    );
    exercise(&work, &case, &environment);
}
