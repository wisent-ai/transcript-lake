//! A native Kimi store identifies a person's turn and an injected message
//! independently of Lake's projection. Only the former is authored prose.

use std::fs;

use serde::Deserialize;
use serde_json::Value;

use super::{evidence, exercise, Expected};

#[derive(Deserialize)]
struct KimiExpected {
    #[serde(flatten)]
    session: Expected,
    injected_text: String,
}

#[test]
#[ignore = "requires a real Kimi store with independently identified user and injected messages"]
fn native_origin_preserves_authorship() {
    let manifest = std::env::var("LAKE_TEST_KIMI_EXPECTATION")
        .expect("set LAKE_TEST_KIMI_EXPECTATION to a real Kimi session expectation");
    let body = fs::read(manifest).expect("read actual-store expectation");
    let expected: KimiExpected = serde_json::from_slice(&body).expect("decode Kimi expectation");
    assert_eq!(expected.session.runtime, "kimi");
    assert!(
        !expected.injected_text.is_empty(),
        "identify a native injection"
    );
    assert_ne!(expected.session.user_text, expected.injected_text);

    let work = evidence::directory().join(uuid::Uuid::new_v4().to_string());
    fs::create_dir_all(&work).expect("create isolated adoption workspace");
    fs::write(work.join("expectation.json"), body).expect("retain acceptance input");
    let events = exercise(&work, &expected.session, &[]);
    assert!(
        events.iter().any(|event: &Value| {
            event["session_id"] == expected.session.session_id
                && event["event_type"] == "user"
                && event["text"] == expected.session.user_text
                && event["extra"]["attribution"] == "user"
        }),
        "the native user origin did not retain explicit authorship"
    );
    assert!(
        events.iter().any(|event| {
            event["session_id"] == expected.session.session_id
                && event["event_type"] == "meta"
                && event["text"] == expected.injected_text
                && event["extra"]["kind"] == "injected"
        }),
        "the native injected message is absent from the persisted Lake"
    );
    assert!(
        !events.iter().any(|event| {
            event["session_id"] == expected.session.session_id
                && event["event_type"] == "user"
                && event["text"] == expected.injected_text
        }),
        "an injection was persisted as the person's writing"
    );
}
