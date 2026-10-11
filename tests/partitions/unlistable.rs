//! Real CLI journey: an events directory that exists and cannot be listed is
//! refused naming it by `compact` and `status`, instead of being read as "no
//! partitions" — which made `compact` tell the operator to start the stream
//! about a lake that holds events. A lake with no events directory, or an
//! empty one, is still no partitions. The lake lives in the retained journey
//! directory under CARGO_TARGET_TMPDIR; every command and its output are kept
//! there.
#[path = "../harnesses/evidence.rs"]
mod evidence;

use std::fs;

#[test]
fn an_unlistable_events_directory_is_refused_by_name() {
    let work = evidence::directory();
    let lake = work.join("lake");
    fs::create_dir_all(&lake).expect("create the journey lake");
    let events = lake.join("events");
    // A file where the events directory belongs: listing it fails.
    fs::write(&events, "not a directory").expect("place a file at the events path");
    let environment = [("LAKE_DATA", lake.as_path())];

    let compact = evidence::run(&work, "compact-unlistable", &["compact"], &environment);
    let said = String::from_utf8_lossy(&compact.stderr);
    assert!(!compact.status.success(), "compact did not refuse: {said}");
    assert!(
        said.contains(&format!("cannot list {}", events.display())),
        "compact did not name the events directory: {said}"
    );
    assert!(
        !said.contains("start the stream first"),
        "compact read the unlistable directory as no partitions: {said}"
    );

    let status = evidence::run(
        &work,
        "status-unlistable",
        &["status", "--json"],
        &environment,
    );
    let said = String::from_utf8_lossy(&status.stderr);
    assert!(!status.status.success(), "status did not refuse: {said}");
    assert!(
        said.contains(&format!("cannot list {}", events.display())),
        "status did not name the events directory: {said}"
    );

    fs::remove_file(&events).expect("remove the placed file");
    fs::create_dir(&events).expect("create an empty events directory");
    let compact = evidence::run(&work, "compact-empty", &["compact"], &environment);
    let said = String::from_utf8_lossy(&compact.stderr);
    assert!(
        said.contains("start the stream first"),
        "an empty lake was not reported as having no partitions: {said}"
    );
}
