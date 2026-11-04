//! Real CLI journey: every `transcript-lake query` statement runs under the
//! memory this machine has available when it starts, a bounded number of
//! DuckDB workers, no insertion order, and a spill directory under the Lake
//! root, never DuckDB's default of 80% of physical memory and one worker per
//! core. The statement reads DuckDB's own settings back, and stderr names the
//! measured limit. The Lake and every command's output stay in the retained
//! journey directory under the checkout's ignored `.build/journeys`.
#[path = "../harnesses/evidence.rs"]
mod evidence;

use std::fs;

const EVENT: &str = r#"{"ts":null,"runtime":"journey","machine":"journey","session_id":"journey-session","project":"journey","event_type":"user","text":"hello","tool_name":null,"model":null,"tokens_in":null,"tokens_out":null,"extra":null}"#;

const SETTINGS: &str = "select current_setting('memory_limit') as memory_limit, \
    current_setting('threads') as threads, \
    current_setting('preserve_insertion_order') as ordered, \
    current_setting('temp_directory') as spill, \
    (select count(*) from events) as events";

/// DuckDB prints a limit in binary units ("22.3 GiB"); the bytes it stands for.
fn bytes(printed: &str) -> f64 {
    let (number, unit) = printed.split_once(' ').expect("a limit with a unit");
    let number: f64 = number.parse().expect("a numeric limit");
    let scale = match unit {
        "KiB" => 1024f64,
        "MiB" => 1024f64.powi(2),
        "GiB" => 1024f64.powi(3),
        "TiB" => 1024f64.powi(4),
        other => panic!("unexpected memory unit {other}"),
    };
    number * scale
}

#[test]
fn a_statement_runs_under_the_memory_available_now() {
    let work = evidence::directory();
    let lake = work.join("lake");
    let partition = lake.join("events/runtime=journey/date=fixture");
    fs::create_dir_all(&partition).expect("create the journey partition");
    fs::write(partition.join("part-journey.ndjson"), format!("{EVENT}\n")).expect("write an event");
    let environment = [("LAKE_DATA", lake.as_path())];

    let answered = evidence::run(
        &work,
        "settings",
        &["query", "--json", SETTINGS],
        &environment,
    );
    let said = String::from_utf8_lossy(&answered.stderr);
    assert!(
        answered.status.success(),
        "the settings statement was refused: {said}"
    );
    let rows: serde_json::Value =
        serde_json::from_slice(&answered.stdout).expect("query --json answers JSON rows");
    let row = &rows[0];

    let limit = bytes(row["memory_limit"].as_str().expect("memory_limit is text"));
    let physical = {
        let output = std::process::Command::new("/usr/sbin/sysctl")
            .args(["-n", "hw.memsize"])
            .output()
            .expect("read hw.memsize");
        String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse::<f64>()
            .expect("hw.memsize is a number")
    };
    assert!(limit > 0.0, "the statement ran with no memory: {row}");
    assert!(
        limit < physical * 0.8,
        "the statement ran under DuckDB's default of 80% of physical memory ({physical} bytes): {row}"
    );
    assert_eq!(
        row["threads"], 4,
        "the statement ran with an unbounded worker count: {row}"
    );
    assert_eq!(
        row["ordered"], false,
        "the statement kept insertion order, which forbids spilling: {row}"
    );
    assert_eq!(
        row["spill"].as_str(),
        Some(lake.join("duckdb-spill").to_string_lossy().as_ref()),
        "the spill directory is not under the Lake root: {row}"
    );
    assert_eq!(
        row["events"], 1,
        "the statement did not read the Lake: {row}"
    );

    assert!(
        said.contains("duckdb memory limit: ") && said.contains("the memory available now ("),
        "stderr does not name the measured limit: {said}"
    );
}
