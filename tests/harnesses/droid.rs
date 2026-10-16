//! Qualify Factory's provider notices against a real native session store.
//! The source remains read-only; adoption and reports stay in the build directory.

use std::fs;

use serde::Deserialize;

use super::{evidence, exercise, Expected};

#[derive(Deserialize)]
struct DroidExpected {
    #[serde(flatten)]
    session: Expected,
    provider_notice_text: String,
}

#[test]
#[ignore = "requires a real Factory store with an independently observed provider notice"]
fn native_provider_notices_are_not_operator_turns() {
    let manifest = std::env::var("LAKE_TEST_DROID_EXPECTATION").expect(
        "set LAKE_TEST_DROID_EXPECTATION to an observed native session and provider notice",
    );
    let body = fs::read(manifest).expect("read native Droid expectation");
    let expected: DroidExpected =
        serde_json::from_slice(&body).expect("parse native Droid expectation");
    assert_eq!(expected.session.runtime, "droid");
    assert!(!expected.session.session_id.is_empty());
    assert!(!expected.session.user_text.is_empty());
    assert!(!expected.provider_notice_text.is_empty());
    let work = evidence::directory();
    fs::write(work.join("expectation.json"), body).expect("retain exact native expectation");
    let events = exercise(&work, &expected.session, &[]);
    assert!(
        events.iter().any(|event| {
            event["session_id"] == expected.session.session_id
                && event["event_type"] == "user"
                && event["text"] == expected.session.user_text
                && event["extra"]["role"] == "user"
        }),
        "a real typed turn retains its native role for downstream attribution"
    );
    assert!(
        events.iter().any(|event| {
            event["session_id"] == expected.session.session_id
                && event["event_type"] == "meta"
                && event["text"] == expected.provider_notice_text
                && event["extra"]["role"] == "user"
                && event["extra"]["visibility"].is_string()
        }),
        "the provider notice and its visibility must remain in the canonical archive"
    );
    assert!(
        !events.iter().any(|event| {
            event["session_id"] == expected.session.session_id
                && event["event_type"] == "user"
                && event["text"] == expected.provider_notice_text
        }),
        "a provider notice must not be a user turn"
    );
}
