//! A corrupt existing archive is a refusal, not permission to discard history.
use std::fs::{self, OpenOptions};
use std::io::Write;

use super::{message, Journey};

#[test]
fn recovery_refuses_corrupt_canonical_evidence_without_advancing_the_cursor() {
    let journey = Journey::new();
    let kept = message(
        "kept",
        "2026-09-12T06:00:02.000Z",
        "Preserve the archived instruction.",
    );
    journey.replace(std::slice::from_ref(&kept));
    journey.adopt("initial");
    let date = fs::read_dir(journey.data.join("events/runtime=omp"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let partition = fs::read_dir(date).unwrap().next().unwrap().unwrap().path();
    let mut file = OpenOptions::new().append(true).open(&partition).unwrap();
    file.write_all(b"damaged canonical record\n").unwrap();
    file.sync_all().unwrap();
    drop(file);
    let canonical_before = fs::read(&partition).unwrap();
    let cursor_before = fs::read(journey.data.join("cursors.json")).unwrap();
    journey.replace(&[
        message(
            "earlier",
            "2026-09-12T06:00:01.000Z",
            "Recover the missed instruction.",
        ),
        kept,
    ]);
    let result = journey.run_adopt("corrupt-canonical-refusal");
    assert!(!result.status.success(), "a corrupt archive was accepted");
    let diagnostic = String::from_utf8_lossy(&result.stderr);
    assert!(
        diagnostic.contains("corrupt canonical partition"),
        "{diagnostic}"
    );
    assert!(
        diagnostic.contains(partition.to_str().unwrap()),
        "{diagnostic}"
    );
    assert_eq!(
        fs::read(&partition).unwrap(),
        canonical_before,
        "recovery altered evidence it could not read"
    );
    assert_eq!(
        fs::read(journey.data.join("cursors.json")).unwrap(),
        cursor_before,
        "the cursor advanced despite refused recovery"
    );
}
