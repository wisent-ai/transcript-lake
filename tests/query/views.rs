//! Real CLI journey: `transcript-lake query` binds only the views a statement
//! references and the views those are written over. A Lake with one event
//! answers `sessions`; an override of the view script whose `tools_daily`
//! section cannot be created leaves every statement that does not name
//! `tools_daily` answered, and refuses the one that does; a section line that
//! names no view is refused naming the line. The Lake, the override and every
//! command's output stay in the retained journey directory under
//! the checkout's ignored `.build/journeys`.
#[path = "../harnesses/evidence.rs"]
mod evidence;

use std::fs;

/// One canonical event of a journey session, every column stated.
const EVENT: &str = r#"{"ts":null,"runtime":"journey","machine":"journey","session_id":"journey-session","project":"journey","event_type":"user","text":"hello","tool_name":null,"model":null,"tokens_in":null,"tokens_out":null,"extra":null}"#;

/// The section the override makes impossible to create.
const SECTION: &str = "-- view: tools_daily after events\n";

/// A statement no Lake view answers, put into the `tools_daily` section.
const BROKEN: &str = "SELECT * FROM a_relation_this_journey_never_creates;\n";

#[test]
fn a_statement_binds_only_the_views_it_names() {
    let work = evidence::directory();
    let lake = work.join("lake");
    let partition = lake.join("events/runtime=journey/date=fixture");
    fs::create_dir_all(&partition).expect("create the journey partition");
    let event = format!("{EVENT}\n");
    fs::write(partition.join("part-journey.ndjson"), event).expect("write an event");

    let views = concat!(env!("CARGO_MANIFEST_DIR"), "/sql/views.sql");
    let canonical = fs::read_to_string(views).expect("read the canonical view script");
    assert!(
        canonical.contains(SECTION),
        "views.sql has no tools_daily section"
    );
    let (first, last) = (canonical.find(SECTION), canonical.rfind(SECTION));
    assert_eq!(first, last, "views.sql has two tools_daily sections");
    let sql = work.join("sql");
    fs::create_dir_all(&sql).expect("create the override directory");
    let broken = canonical.replace(SECTION, &format!("{SECTION}{BROKEN}"));
    fs::write(sql.join("views.sql"), broken).expect("write the override");
    let environment = [
        ("LAKE_DATA", lake.as_path()),
        ("TRANSCRIPT_LAKE_SQL", sql.as_path()),
    ];

    let select_one = ["query", "--json", "select 1 as one"];
    let one = evidence::run(&work, "select-one", &select_one, &environment);
    let said = String::from_utf8_lossy(&one.stderr);
    assert!(
        one.status.success(),
        "select 1 bound a view it does not name: {said}"
    );

    let statement = ["query", "--json", "select session_id from sessions"];
    let sessions = evidence::run(&work, "sessions", &statement, &environment);
    let said = String::from_utf8_lossy(&sessions.stderr);
    assert!(sessions.status.success(), "sessions was refused: {said}");
    let answered = String::from_utf8_lossy(&sessions.stdout);
    assert!(
        answered.contains("journey-session"),
        "sessions did not read the event: {answered}"
    );

    let statement = ["query", "--json", "select * from tools_daily"];
    let tools = evidence::run(&work, "tools-daily", &statement, &environment);
    let said = String::from_utf8_lossy(&tools.stderr);
    assert!(
        !tools.status.success(),
        "the broken tools_daily section was not bound: {said}"
    );
    assert!(
        said.contains("a_relation_this_journey_never_creates"),
        "the refusal does not name the failed relation: {said}"
    );

    let nameless = canonical.replace(SECTION, "-- view: \n");
    fs::write(sql.join("views.sql"), nameless).expect("write a nameless section");
    let refused = evidence::run(&work, "nameless", &select_one, &environment);
    let said = String::from_utf8_lossy(&refused.stderr);
    assert!(
        !refused.status.success(),
        "a section line naming no view was accepted: {said}"
    );
    assert!(
        said.contains("names no view"),
        "the refusal does not name the cause: {said}"
    );
}
