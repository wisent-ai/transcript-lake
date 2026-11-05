//! Real CLI journey: `transcript-lake query --stdin` runs a statement longer
//! than the argument list the system allows a program (`getconf ARG_MAX`),
//! read from standard input, and DuckDB receives it the same way, so neither
//! hop is refused with `Argument list too long`. Oko's catalogue sync names
//! every changed session in one statement, and once the catalogue held many
//! sessions that statement no longer fit an argument. The same statement given
//! as an argument is refused by the system, which proves its length. Refused
//! too: `--stdin` with SQL in the arguments, and empty standard input. The
//! Lake and every command's output stay in the retained journey directory
//! under the checkout's ignored `.build/journeys`.
#[path = "../harnesses/evidence.rs"]
mod evidence;

use std::fs;
use std::io::Write;
use std::process::{Command, Stdio};

const EVENT: &str = r#"{"ts":null,"runtime":"journey","machine":"journey","session_id":"journey-session","project":"journey","event_type":"user","text":"hello","tool_name":null,"model":null,"tokens_in":null,"tokens_out":null,"extra":null}"#;

fn arg_max() -> usize {
    let output = Command::new("getconf").arg("ARG_MAX").output().expect("getconf ARG_MAX");
    String::from_utf8_lossy(&output.stdout).trim().parse().expect("ARG_MAX is a number")
}

/// A statement over the Lake whose `in (…)` list of session ids is longer
/// than `bytes`, matching the one real session.
fn long_statement(bytes: usize) -> String {
    let mut ids = vec!["'journey-session'".to_string()];
    let mut length = 0;
    let mut n = 0;
    while length <= bytes {
        let id = format!("'absent-session-{n:012}'");
        length += id.len() + 1;
        ids.push(id);
        n += 1;
    }
    format!("select count(*) as events from events where session_id in ({})", ids.join(","))
}

fn fed(lake: &std::path::Path, args: &[&str], input: &[u8]) -> std::process::Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_transcript-lake"))
        .args(args)
        .env("LAKE_DATA", lake)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("start transcript-lake");
    child.stdin.take().unwrap().write_all(input).expect("write the statement to standard input");
    child.wait_with_output().expect("transcript-lake ends")
}

#[test]
fn a_statement_longer_than_the_argument_list_runs_from_standard_input() {
    let work = evidence::directory();
    let lake = work.join("lake");
    let partition = lake.join("events/runtime=journey/date=fixture");
    fs::create_dir_all(&partition).expect("create the journey partition");
    fs::write(partition.join("part-journey.ndjson"), format!("{EVENT}\n")).expect("write an event");
    let statement = long_statement(arg_max());
    fs::write(work.join("statement.sql"), &statement).expect("retain the statement");

    let as_argument = Command::new(env!("CARGO_BIN_EXE_transcript-lake"))
        .args(["query", "--json", &statement])
        .env("LAKE_DATA", &lake)
        .output();
    let refused = match as_argument {
        Err(error) => error.to_string(),
        Ok(output) => panic!(
            "a statement of {} bytes was accepted as an argument, so this run proves nothing about --stdin: {}",
            statement.len(),
            String::from_utf8_lossy(&output.stderr)
        ),
    };
    assert!(refused.contains("Argument list too long"), "the system refused the argument for another reason: {refused}");

    let answered = fed(&lake, &["query", "--json", "--stdin"], statement.as_bytes());
    let said = String::from_utf8_lossy(&answered.stderr);
    fs::write(work.join("stdin.stderr"), said.as_bytes()).unwrap();
    assert!(answered.status.success(), "query --stdin was refused: {said}");
    let rows: serde_json::Value = serde_json::from_slice(&answered.stdout).expect("query --json answers JSON rows");
    assert_eq!(rows[0]["events"], 1, "the long statement did not read the Lake: {rows}");

    let both = fed(&lake, &["query", "--stdin", "select 1"], b"select 2");
    assert!(!both.status.success(), "--stdin with SQL in the arguments was not refused");
    assert!(
        String::from_utf8_lossy(&both.stderr).contains("takes none in its arguments"),
        "the refusal does not say --stdin takes no SQL argument: {}",
        String::from_utf8_lossy(&both.stderr)
    );
    let empty = fed(&lake, &["query", "--stdin"], b"  \n");
    assert!(!empty.status.success(), "an empty statement on standard input was not refused");
    assert!(String::from_utf8_lossy(&empty.stderr).contains("usage: transcript-lake query"), "{}", String::from_utf8_lossy(&empty.stderr));
}
