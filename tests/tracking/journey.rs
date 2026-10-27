//! Adopt genuine native executions produced by Jeden's tracking journey.
use std::{fs, path::PathBuf};

use serde::Deserialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{evidence, exercise, Expected};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Capture {
    source_home: PathBuf,
    root: PathBuf,
    cases: Vec<Case>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Case {
    session_id: String,
    transcript: PathBuf,
    transcript_sha256: String,
    user_text: String,
    file: String,
    expected_content: Option<String>,
    refused_read: bool,
    model_event_id: String,
}

fn native(case: &Case) -> Vec<Value> {
    let bytes = fs::read(&case.transcript).expect("read genuine native journal");
    assert_eq!(
        format!("{:x}", Sha256::digest(&bytes)),
        case.transcript_sha256,
        "the native fixture changed after the real execution"
    );
    std::str::from_utf8(&bytes)
        .expect("UTF-8 native journal")
        .lines()
        .map(|line| serde_json::from_str(line).expect("native event JSON"))
        .collect()
}

#[test]
#[ignore = "requires a genuine successful Jeden tracking capture and the declared secret formats"]
fn native_execution_survives_adoption() {
    let path = std::env::var("LAKE_TEST_TRACKING_CAPTURE")
        .expect("set LAKE_TEST_TRACKING_CAPTURE to Jeden's actual tracking-capture.json");
    let bytes = fs::read(path).expect("read real execution capture");
    let capture: Capture = serde_json::from_slice(&bytes).expect("decode execution capture");
    assert!(
        capture.cases.iter().any(|case| case.refused_read),
        "capture needs a real refused read"
    );
    assert!(
        capture
            .cases
            .iter()
            .any(|case| case.expected_content.is_some()),
        "capture needs a real file read"
    );
    for case in &capture.cases {
        let original = native(case);
        let work = evidence::directory().join(uuid::Uuid::new_v4().to_string());
        fs::create_dir_all(&work).expect("create isolated archive destination");
        fs::write(work.join("capture.json"), &bytes).expect("retain independent acceptance input");
        let expected = Expected {
            runtime: "jeden".into(),
            root: capture.root.clone(),
            session_id: case.session_id.clone(),
            user_text: case.user_text.clone(),
        };
        let rows = exercise(&work, &expected, &[("HOME", capture.source_home.as_path())]);
        let rows: Vec<&Value> = rows
            .iter()
            .filter(|row| row["session_id"] == case.session_id)
            .collect();
        let receipts: Vec<&Value> = original
            .iter()
            .filter(|row| row["payload"]["type"] == "interaction")
            .collect();
        let metadata: Vec<&Value> = rows
            .iter()
            .copied()
            .filter(|row| row["event_type"] == "meta" && row["extra"]["kind"] == "interaction")
            .collect();
        assert_eq!(
            metadata.len(),
            receipts.len(),
            "an actual execution record was lost or duplicated"
        );
        for source in receipts {
            let matching: Vec<&Value> = metadata
                .iter()
                .copied()
                .filter(|row| row["extra"]["sequence"] == source["sequence"])
                .collect();
            let [row] = matching.as_slice() else {
                panic!("execution sequence needs exactly one canonical row")
            };
            let text = row["text"]
                .as_str()
                .expect("execution metadata must retain its observed payload");
            let body: Value = serde_json::from_str(text).expect("masked execution payload JSON");
            let data = &source["payload"]["data"];
            assert_eq!(body["type"], data["type"]);
            assert_eq!(body["operationKind"], data["operationKind"]);
            assert_eq!(row["extra"]["recordType"], data["type"]);
            if data["type"] == "operation_result" {
                assert_eq!(body["outcome"]["status"], data["outcome"]["status"]);
                assert_eq!(row["extra"]["requestEvent"], data["requestEvent"]);
                assert!(
                    metadata.iter().any(|request| request["extra"]["eventId"]
                        == row["extra"]["requestEvent"]
                        && request["extra"]["recordType"] == "operation_request"),
                    "terminal result lost its actual request"
                );
            }
        }
        let decision = metadata
            .iter()
            .find(|row| row["extra"]["eventId"] == case.model_event_id)
            .expect("the model's actual recorded decision must survive adoption");
        let body: Value =
            serde_json::from_str(decision["text"].as_str().expect("model decision text"))
                .expect("model decision JSON");
        assert!(
            body["outcome"]["output"]["content"]
                .as_str()
                .expect("returned model content")
                .contains(&case.file),
            "the model's actual read decision was discarded"
        );
        let calls: Vec<&Value> = rows
            .iter()
            .copied()
            .filter(|row| {
                row["event_type"] == "tool_call"
                    && row["tool_name"] == "read_file"
                    && row["text"]
                        .as_str()
                        .is_some_and(|text| text.contains(&case.file))
            })
            .collect();
        assert!(
            !calls.is_empty(),
            "the real conversational read was not adopted"
        );
        for call in calls {
            let results: Vec<&Value> = rows
                .iter()
                .copied()
                .filter(|row| {
                    row["event_type"] == "tool_result"
                        && row["extra"]["tool_call_id"] == call["extra"]["tool_call_id"]
                })
                .collect();
            let [result] = results.as_slice() else {
                panic!("read needs its own exact result")
            };
            assert_eq!(
                result["extra"]["is_error"].as_bool(),
                Some(case.refused_read)
            );
            if let Some(content) = &case.expected_content {
                assert!(result["text"]
                    .as_str()
                    .expect("actual read result text")
                    .contains(content));
            }
        }
        assert_eq!(
            rows.iter()
                .filter(|row| row["event_type"] == "user" && row["text"] == case.user_text)
                .count(),
            original
                .iter()
                .filter(|row| row["payload"]["type"] == "user"
                    && row["payload"]["data"]["task"] == case.user_text)
                .count(),
            "execution metadata fabricated an operator instruction"
        );
        assert_eq!(
            native(case),
            original,
            "archive adoption must not alter the genuine source journal"
        );
    }
}
