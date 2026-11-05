//! Real CLI journey: two `transcript-lake query` statements on one Lake root
//! do not run DuckDB at the same time. Each measures the memory available
//! when it starts and takes it all as its limit, so statements started
//! together each took the whole machine: seven Oko readers held fourteen to
//! seventeen gigabytes each on a 64 GB machine and filled swap. The second
//! statement waits on the root's `duckdb-statement.lock`, says on stderr
//! whom it waits for, and starts after the first ends. A statement on
//! another Lake root does not wait. The Lakes and every command's output stay
//! in the retained journey directory under the checkout's ignored
//! `.build/journeys`.
#[path = "../harnesses/evidence.rs"]
mod evidence;

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const EVENT: &str = r#"{"ts":null,"runtime":"journey","machine":"journey","session_id":"journey-session","project":"journey","event_type":"user","text":"hello","tool_name":null,"model":null,"tokens_in":null,"tokens_out":null,"extra":null}"#;

/// A statement that keeps DuckDB busy for a few seconds and reads the Lake.
const LONG: &str = "select sum(i % 7) as busy, (select count(*) from events) as events from range(1500000000) t(i)";
const SHORT: &str = "select count(*) as events from events";

fn lake(root: &Path) {
    let partition = root.join("events/runtime=journey/date=fixture");
    fs::create_dir_all(&partition).expect("create the journey partition");
    fs::write(partition.join("part-journey.ndjson"), format!("{EVENT}\n")).expect("write an event");
}

fn started(work: &Path, name: &str, lake: &Path, sql: &str) -> std::process::Child {
    Command::new(env!("CARGO_BIN_EXE_transcript-lake"))
        .args(["query", "--json", sql])
        .env("LAKE_DATA", lake)
        .stdout(fs::File::create(work.join(format!("{name}.stdout"))).unwrap())
        .stderr(fs::File::create(work.join(format!("{name}.stderr"))).unwrap())
        .stdin(Stdio::null())
        .spawn()
        .expect("start transcript-lake query")
}

fn statement_running(lake: &Path) -> bool {
    let holder = fs::read_to_string(lake.join("duckdb-statement.lock")).unwrap_or_default();
    holder.starts_with("pid ")
}

#[test]
fn statements_on_one_lake_run_one_at_a_time() {
    let work = evidence::directory();
    let shared = work.join("shared-lake");
    let other = work.join("other-lake");
    lake(&shared);
    lake(&other);

    let mut first = started(&work, "first", &shared, LONG);
    let deadline = Instant::now() + Duration::from_secs(60);
    while !statement_running(&shared) {
        assert!(Instant::now() < deadline, "the first statement never took the Lake's statement lock");
        std::thread::sleep(Duration::from_millis(50));
    }
    let elsewhere = Instant::now();
    let separate = evidence::run(&work, "other-root", &["query", "--json", SHORT], &[("LAKE_DATA", other.as_path())]);
    let elsewhere = elsewhere.elapsed();
    assert!(separate.status.success(), "the statement on another Lake root was refused: {}", String::from_utf8_lossy(&separate.stderr));
    assert!(
        first.try_wait().unwrap().is_none(),
        "the first statement ended before the other root's statement did, so this run proves nothing about waiting"
    );

    let mut second = started(&work, "second", &shared, SHORT);
    let first_status = first.wait().expect("the first statement ends");
    let first_ended = Instant::now();
    let second_status = second.wait().expect("the second statement ends");
    assert!(first_status.success(), "the first statement was refused: {}", fs::read_to_string(work.join("first.stderr")).unwrap());
    assert!(second_status.success(), "the second statement was refused: {}", fs::read_to_string(work.join("second.stderr")).unwrap());

    let said = fs::read_to_string(work.join("second.stderr")).unwrap();
    let pid = first.id();
    assert!(
        said.contains("czekam: the DuckDB statement of pid ") && said.contains(&format!("pid {pid} ")),
        "the second statement did not say it waits for the first (pid {pid}): {said}"
    );
    assert!(said.contains("rodzaj: blokada"), "the wait is not named a lock wait: {said}");
    let measured = said.find("duckdb memory limit: ").expect("the second statement names its limit");
    let waited = said.find("koniec czekania: the DuckDB statement of").expect("the wait ends");
    assert!(waited < measured, "the second statement measured memory before the first ended: {said}");
    let rows: serde_json::Value = serde_json::from_str(&fs::read_to_string(work.join("second.stdout")).unwrap()).unwrap();
    assert_eq!(rows[0]["events"], 1, "the second statement did not read the Lake: {rows}");
    assert!(
        !fs::read_to_string(work.join("other-root.stderr")).unwrap_or_default().contains("czekam: the DuckDB statement"),
        "a statement on another Lake root waited for this root's statement (took {elsewhere:?})"
    );
    eprintln!("first ended {:?} before the second", first_ended.elapsed());
}
