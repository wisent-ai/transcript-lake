//! A native OMP advisor update is retained as the agent's meta record, never
//! as an operator turn, while a real operator turn stays a user event; `show`
//! exposes the advisor record with its native sender, and an unowned root is
//! refused. The sessions are named by an independently observed expectation
//! (`LAKE_TEST_OMP_AUTHORSHIP`) and adopted, with the secret formats
//! declaration adopt requires, from a home of the case's own.
#[path = "../harnesses/evidence.rs"]
mod evidence;
mod store;

use serde_json::Value;
use std::{fs, path::PathBuf};

#[derive(serde::Deserialize)]
struct Authorship {
    authored_file: PathBuf,
    authored_session: String,
    authored_text: String,
    generated_file: PathBuf,
    generated_session: String,
    generated_text: String,
}

#[test]
#[ignore = "requires independently identified real OMP operator and advisor records"]
fn native_advisor_updates_are_not_operator_turns() {
    let manifest = std::env::var("LAKE_TEST_OMP_AUTHORSHIP").expect(
        "set LAKE_TEST_OMP_AUTHORSHIP to an independently observed native session expectation",
    );
    let body = fs::read(manifest).expect("read OMP authorship expectation");
    let expected: Authorship =
        serde_json::from_slice(&body).expect("decode OMP authorship expectation");
    assert!(
        !expected.authored_text.is_empty(),
        "name a real operator turn"
    );
    assert!(
        !expected.generated_text.is_empty(),
        "name a real advisor update"
    );
    let work = evidence::directory();
    fs::write(work.join("expectation.json"), body).expect("retain acceptance input");
    let home = work.join("home");
    let project = store::under(&home).join(store::PROJECT);
    fs::create_dir_all(&project).expect("create isolated native-source project");
    for source in [&expected.authored_file, &expected.generated_file] {
        let name = source.file_name().expect("native source file has a name");
        fs::copy(source, project.join(name))
            .expect("copy unchanged native transcript into isolated project");
    }
    let lake = work.join("lake");
    let formats = evidence::no_secret_classes(&work);
    let adopted = evidence::run(
        &work,
        "adopt-authorship",
        &[
            "--data-dir",
            lake.to_str().expect("UTF-8 Lake path"),
            "adopt",
            "--source",
            "omp",
            "--root",
            project.to_str().expect("UTF-8 isolated project"),
            "--json",
        ],
        &[
            ("HOME", home.as_path()),
            ("TRANSCRIPT_LAKE_SECRET_FORMATS", formats.as_path()),
        ],
    );
    assert!(
        adopted.status.success(),
        "{}",
        String::from_utf8_lossy(&adopted.stderr)
    );
    let events = store::rows(&lake);
    assert!(
        events.iter().any(|event| {
            event["session_id"] == expected.authored_session
                && event["event_type"] == "user"
                && event["text"]
                    .as_str()
                    .is_some_and(|text| text.contains(&expected.authored_text))
        }),
        "independently identified operator turn was not persisted as a user event"
    );
    assert!(
        events.iter().any(|event| {
            event["session_id"] == expected.generated_session
                && event["event_type"] == "meta"
                && event["extra"]["role"] == "user"
                && event["extra"]["synthetic"] == true
                && event["extra"]["attribution"] == "agent"
                && event["text"]
                    .as_str()
                    .is_some_and(|text| text.contains(&expected.generated_text))
        }),
        "native advisor update was not retained with its recorded provenance"
    );
    assert!(
        !events.iter().any(|event| {
            event["event_type"] == "user"
                && event["text"]
                    .as_str()
                    .is_some_and(|text| text.contains(&expected.generated_text))
        }),
        "agent-authored advisor update was attributed to the operator"
    );
    let shown = evidence::run(
        &work,
        "show-authorship",
        &[
            "--data-dir",
            lake.to_str().expect("UTF-8 Lake path"),
            "show",
            &expected.generated_session,
            "--include",
            "user,meta",
            "--json",
        ],
        &[],
    );
    assert!(
        shown.status.success(),
        "{}",
        String::from_utf8_lossy(&shown.stderr)
    );
    let report: Value = serde_json::from_slice(&shown.stdout).expect("decode actual show report");
    assert!(
        report["events"]
            .as_array()
            .expect("show events")
            .iter()
            .any(|event| {
                event["event_type"] == "meta"
                    && event["extra"]["synthetic"] == true
                    && event["extra"]["attribution"] == "agent"
                    && event["text"]
                        .as_str()
                        .is_some_and(|text| text.contains(&expected.generated_text))
            }),
        "show did not expose the advisor record and its native sender"
    );
    let refused = evidence::run(
        &work,
        "refused-authorship",
        &[
            "--data-dir",
            work.join("refused")
                .to_str()
                .expect("UTF-8 refusal destination"),
            "adopt",
            "--source",
            "omp",
            "--root",
            work.to_str().expect("UTF-8 unowned root"),
            "--json",
        ],
        &[("TRANSCRIPT_LAKE_SECRET_FORMATS", formats.as_path())],
    );
    assert!(
        !refused.status.success(),
        "an unowned source root must be refused"
    );
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("not a discovered"),
        "unexpected refusal: {}",
        String::from_utf8_lossy(&refused.stderr)
    );
}
