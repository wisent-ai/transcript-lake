//! A real Qwen native store: only a source-recorded real_user turn becomes
//! authored prose; goal/runtime input and tool output do not.

use std::fs;

use serde::Deserialize;
use serde_json::Value;

use super::{evidence, exercise, Expected};

#[derive(Deserialize)]
struct QwenExpected {
    #[serde(flatten)]
    session: Expected,
    tool_text: String,
    authored_text: String,
    synthetic_text: String,
    synthetic_provenance: String,
}

#[test]
#[ignore = "requires a real Qwen store with independently identified authored, synthetic and tool turns"]
fn native_provenance_keeps_author_and_excludes_automation() {
    let manifest = std::env::var("LAKE_TEST_QWEN_EXPECTATION")
        .expect("set LAKE_TEST_QWEN_EXPECTATION to a real Qwen session expectation");
    let body = fs::read(manifest).expect("read actual-store expectation");
    let expected: QwenExpected = serde_json::from_slice(&body).expect("decode Qwen expectation");
    assert_eq!(expected.session.runtime, "qwen");
    assert!(!expected.tool_text.is_empty(), "name a real tool result");
    assert!(!expected.authored_text.is_empty(), "name real author text");
    assert!(
        !expected.synthetic_text.is_empty(),
        "name recorded non-author text"
    );
    assert_ne!(expected.synthetic_provenance, "real_user");

    let work = evidence::directory().join(uuid::Uuid::new_v4().to_string());
    fs::create_dir_all(&work).expect("create isolated adoption workspace");
    fs::write(work.join("expectation.json"), body).expect("retain acceptance input");
    let events = exercise(&work, &expected.session, &[]);
    let matches = |event: &Value| {
        event["runtime"] == "qwen"
            && event["session_id"] == expected.session.session_id
            && event["text"] == expected.tool_text
    };
    assert!(
        events
            .iter()
            .any(|event| matches(event) && event["event_type"] == "tool_result"),
        "the independently identified native tool result was not persisted as a tool result"
    );
    assert!(
        !events
            .iter()
            .any(|event| matches(event) && event["event_type"] == "user"),
        "a native tool result was persisted as a person's writing"
    );
    assert!(
        events.iter().any(|event| {
            event["runtime"] == "qwen"
                && event["session_id"] == expected.session.session_id
                && event["event_type"] == "user"
                && event["extra"]["attribution"] == "user"
                && event["extra"]["provenance"] == "real_user"
                && event["text"]
                    .as_str()
                    .is_some_and(|text| text.contains(&expected.authored_text))
        }),
        "the source-recorded real user turn was not imported as authored"
    );
    assert!(
        events.iter().any(|event| {
            event["runtime"] == "qwen"
                && event["session_id"] == expected.session.session_id
                && event["event_type"] == "meta"
                && event["extra"]["provenance"] == expected.synthetic_provenance
                && event["text"]
                    .as_str()
                    .is_some_and(|text| text.contains(&expected.synthetic_text))
        }),
        "the independently identified non-author turn was not retained as metadata"
    );
    assert!(
        events.iter().all(|event| {
            event["event_type"] != "user"
                || !event["text"]
                    .as_str()
                    .is_some_and(|text| text.contains(&expected.synthetic_text))
        }),
        "native non-author input was attributed to the user"
    );
}
