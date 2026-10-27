use serde_json::Value;
use std::{path::Path, process::Output};

use super::evidence;

/// One read-only `doctor --json` of `lake` under `work`'s home and data with
/// `declaration` as the masking policy: the process output, the report and
/// its masking-policy check.
pub(super) fn masking(
    work: &Path,
    lake: &Path,
    declaration: &Path,
    name: &str,
) -> (Output, Value, Value) {
    let data = work.join("data");
    let output = evidence::run(
        work,
        name,
        &[
            "--data-dir",
            lake.to_str().expect("UTF-8 Lake path"),
            "doctor",
            "--json",
        ],
        &[
            ("HOME", work),
            ("XDG_DATA_HOME", &data),
            ("TRANSCRIPT_LAKE_SECRET_FORMATS", declaration),
        ],
    );
    let report: Value = serde_json::from_slice(&output.stdout).expect("read doctor report");
    let policy = report["checks"]
        .as_array()
        .expect("doctor checks")
        .iter()
        .find(|check| check["name"] == "masking-policy")
        .expect("masking readiness check")
        .clone();
    (output, report, policy)
}

pub(super) fn refused(
    work: &Path,
    lake: &Path,
    declaration: &Path,
    name: &str,
    expected: &str,
) {
    let (output, report, policy) = masking(work, lake, declaration, name);
    assert!(!output.status.success(), "invalid masking policy reported healthy");
    assert_eq!(report["healthy"], false);
    assert_eq!(policy["status"], "error");
    let detail = policy["detail"].as_str().expect("masking cause");
    assert!(detail.contains(expected), "doctor lost cause {expected}: {detail}");
    if !declaration.as_os_str().is_empty() {
        assert!(
            detail.contains(&declaration.display().to_string()),
            "doctor lost declaration path: {detail}"
        );
    }
    assert!(!lake.exists(), "read-only doctor created the Lake");
}
