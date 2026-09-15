//! A failed later partition must not turn a replay checkpoint into an append.
use std::fs;

use super::{message, Journey};

#[test]
fn interrupted_recovery_resumes_without_duplicating_retained_history() {
    let journey = Journey::new();
    // Span the documented 512-event commit boundary with distinct real turns.
    let words: Vec<String> = (0..1200)
        .map(|index| format!("Keep operator instruction {index}."))
        .collect();
    let kept: Vec<_> = words
        .iter()
        .enumerate()
        .map(|(index, text)| message(&format!("kept-{index}"), "2026-09-12T06:00:02.000Z", text))
        .collect();
    journey.replace(&kept);
    journey.adopt("initial");

    let earlier = "Recover the earlier operator instruction.";
    let later = "Finish the next day's work too.";
    let mut replaced = vec![message("earlier", "2026-09-12T06:00:01.000Z", earlier)];
    replaced.extend(kept);
    replaced.push(message("later", "2026-09-13T06:00:01.000Z", later));
    journey.replace(&replaced);
    // This is an actual filesystem error, after earlier batches can commit.
    let blocked_partition = journey.data.join("events/runtime=omp/date=2026-09-13");
    fs::write(&blocked_partition, "not a partition directory\n").unwrap();
    let result = journey.run_adopt("interrupted-recovery");
    assert!(
        !result.status.success(),
        "the unwritable partition was accepted"
    );
    fs::remove_file(&blocked_partition).unwrap();
    let diagnostic = String::from_utf8_lossy(&result.stderr);
    assert!(
        diagnostic.contains(blocked_partition.to_str().unwrap()),
        "{diagnostic}"
    );

    journey.adopt("resumed-recovery");
    let mut expected: Vec<&str> = words.iter().map(String::as_str).collect();
    expected.extend([earlier, later]);
    journey.assert_turns(&expected);
    journey.adopt("repeated");
    journey.assert_turns(&expected);
}
