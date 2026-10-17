//! Real CLI journey: a wrong invocation of the real binary (an unknown
//! command, a repeated global `--data-dir`, a `--data-dir` with no path) is
//! refused with the usage status README.md states, the error and the usage
//! on stderr and nothing on stdout, while `--help` succeeds and prints the
//! usage. The status is read from README.md, so the documentation and the
//! binary cannot drift apart unnoticed. Inputs, outputs and the source
//! revision are retained under the package's build directory by `evidence`;
//! with `TRANSCRIPT_LAKE_JOURNEY_REPORT` set, the passed journey also writes
//! the criterion report `oko defects repaired --evidence` reads.
#[path = "../harnesses/evidence.rs"]
mod evidence;

use std::path::Path;

use serde_json::json;

const README: &str = include_str!("../../README.md");
const DOCUMENTED: &str = "prints the error and the usage and exits ";
const USAGE_LINE: &str = "Usage: transcript-lake";

/// The exit status README.md states for a wrong invocation.
fn documented_usage_status() -> i32 {
    let (_, stated) = README
        .split_once(DOCUMENTED)
        .expect("README.md states the exit status of a wrong invocation");
    let digits: String = stated.chars().take_while(char::is_ascii_digit).collect();
    digits
        .parse()
        .expect("README.md states the usage exit status as a number")
}

#[test]
fn a_wrong_invocation_exits_with_the_documented_usage_status() {
    let work = evidence::directory();
    let lake = work.join("lake");
    let lake_arg = lake.to_str().expect("UTF-8 Lake path");
    let environment: &[(&str, &Path)] = &[("HOME", work.as_path())];
    let usage = documented_usage_status();
    let wrong: Vec<(&str, Vec<&str>, &str)> = vec![
        (
            "unknown-command",
            vec!["--data-dir", lake_arg, "no-such-command"],
            "unknown command: no-such-command",
        ),
        (
            "repeated-data-dir",
            vec!["--data-dir", lake_arg, "--data-dir", lake_arg, "status"],
            "duplicate global --data-dir",
        ),
        (
            "pathless-data-dir",
            vec!["--data-dir"],
            "--data-dir requires a path",
        ),
    ];
    let mut refused = Vec::new();
    for (name, args, error) in &wrong {
        let output = evidence::run(&work, name, args, environment);
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        assert_eq!(
            output.status.code(),
            Some(usage),
            "{name} did not exit with the documented usage status: {stderr}"
        );
        assert!(
            stderr.contains(&format!("error: {error}")),
            "{name} did not name its error: {stderr}"
        );
        assert!(
            stderr.contains(USAGE_LINE),
            "{name} did not print the usage on stderr: {stderr}"
        );
        assert!(
            output.stdout.is_empty(),
            "{name} wrote to stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        );
        assert!(
            !lake.exists(),
            "{name} created the Lake it was refused before reaching"
        );
        refused.push(json!({
            "args": args,
            "exit": output.status.code(),
            "stderr": stderr,
        }));
    }

    let help_args = ["--help"];
    let help = evidence::run(&work, "help", &help_args, environment);
    assert!(
        help.status.success(),
        "--help failed: {}",
        String::from_utf8_lossy(&help.stderr)
    );
    assert!(
        String::from_utf8_lossy(&help.stdout).contains(USAGE_LINE),
        "--help did not print the usage"
    );

    evidence::criterion_report(
        include_bytes!("usage.rs"),
        vec![evidence::command(&help_args, &help)],
        json!({ "usageStatus": usage, "refused": refused }),
    );
}
