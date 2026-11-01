//! A continued OMP session copies every message of the session it continues,
//! under that session's own entry ids. Adopting both real transcripts keeps
//! `extra.parent_session` on the continued session's events and each
//! message's `extra.eventId`, so the copies are exactly the continued
//! session's events whose `eventId` its parent also holds, and what was said
//! in the continued session is the rest. The two sessions are named by an
//! independently observed expectation (`LAKE_TEST_OMP_CONTINUATION`) and
//! adopted, with the secret formats declaration adopt requires, from a home
//! of the case's own.
#[path = "../harnesses/evidence.rs"]
mod evidence;
mod store;

use std::collections::HashSet;
use std::{fs, path::PathBuf};

use serde_json::Value;

#[derive(serde::Deserialize)]
struct Continuation {
    parent_file: PathBuf,
    parent_session: String,
    continued_file: PathBuf,
    continued_session: String,
    /// User turns the parent recorded, counted independently from its file.
    parent_user_turns: usize,
    /// User turns said in the continued session itself, counted
    /// independently from its file.
    continued_own_user_turns: usize,
}

fn user_ids<'a>(events: &'a [Value], session: &str) -> Vec<&'a str> {
    events
        .iter()
        .filter(|event| event["session_id"] == session && event["event_type"] == "user")
        .map(|event| event["extra"]["eventId"].as_str().expect("every OMP user event keeps its entry id"))
        .collect()
}

#[test]
#[ignore = "requires an independently identified real OMP continued session and its parent"]
fn a_continued_session_names_its_parent_and_keeps_copied_entry_ids() {
    let manifest = std::env::var("LAKE_TEST_OMP_CONTINUATION")
        .expect("set LAKE_TEST_OMP_CONTINUATION to an independently observed continuation expectation");
    let body = fs::read(manifest).expect("read OMP continuation expectation");
    let expected: Continuation = serde_json::from_slice(&body).expect("decode OMP continuation expectation");
    assert_ne!(expected.parent_session, expected.continued_session, "name two different sessions");
    let work = evidence::directory();
    fs::write(work.join("expectation.json"), body).expect("retain acceptance input");
    let home = work.join("home");
    let project = store::under(&home).join(store::PROJECT);
    fs::create_dir_all(&project).expect("create isolated native-source project");
    for source in [&expected.parent_file, &expected.continued_file] {
        let name = source.file_name().expect("native source file has a name");
        fs::copy(source, project.join(name)).expect("copy unchanged native transcript into isolated project");
    }
    let lake = work.join("lake");
    let formats = evidence::no_secret_classes(&work);
    let adopted = evidence::run(
        &work,
        "adopt-continuation",
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
        &[("HOME", home.as_path()), ("TRANSCRIPT_LAKE_SECRET_FORMATS", formats.as_path())],
    );
    assert!(adopted.status.success(), "{}", String::from_utf8_lossy(&adopted.stderr));
    let events = store::rows(&lake);

    assert!(
        events
            .iter()
            .filter(|event| event["session_id"] == expected.continued_session)
            .all(|event| event["extra"]["parent_session"] == expected.parent_session.as_str()),
        "every event of the continued session names its parent"
    );
    assert!(
        events
            .iter()
            .filter(|event| event["session_id"] == expected.parent_session)
            .all(|event| event["extra"]["parent_session"] != expected.continued_session.as_str()),
        "the parent does not name the session that continued it"
    );

    let parent: HashSet<&str> = user_ids(&events, &expected.parent_session).into_iter().collect();
    let continued = user_ids(&events, &expected.continued_session);
    assert_eq!(parent.len(), expected.parent_user_turns, "the parent's own user turns, each once");
    let copied = continued.iter().filter(|id| parent.contains(**id)).count();
    let own = continued.len() - copied;
    assert_eq!(copied, expected.parent_user_turns, "the continued session copies every parent turn under its id");
    assert_eq!(own, expected.continued_own_user_turns, "what was said in the continued session itself");

    // The same count through the query surface a consumer reads.
    let sql = format!(
        "select count(*) as own from events own where own.session_id = '{continued}' and own.event_type = 'user' \
         and not exists (select 1 from events parent where parent.session_id = \
         json_extract_string(own.extra, '$.parent_session') and json_extract_string(parent.extra, '$.eventId') = \
         json_extract_string(own.extra, '$.eventId'))",
        continued = expected.continued_session,
    );
    let queried = evidence::run(
        &work,
        "query-own-turns",
        &["--data-dir", lake.to_str().expect("UTF-8 Lake path"), "query", "--json", &sql],
        &[],
    );
    assert!(queried.status.success(), "{}", String::from_utf8_lossy(&queried.stderr));
    let rows: Value = serde_json::from_slice(&queried.stdout).expect("decode query answer");
    assert_eq!(
        rows[0]["own"].as_u64(),
        Some(expected.continued_own_user_turns as u64),
        "query reads the continued session's own turns: {rows}"
    );
}
