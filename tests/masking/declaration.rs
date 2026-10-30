//! Real adoption rejects omitted masking classes before publishing events.
#[path = "doctor.rs"]
mod doctor;
#[path = "../harnesses/evidence.rs"]
mod evidence;

use serde_json::{json, Value};
use std::{fs, path::Path};

fn events(root: &Path) -> Vec<Value> {
    let mut found = Vec::new();
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return found,
        Err(error) => panic!("cannot list {}: {error}", root.display()),
    };
    for entry in entries {
        let entry = entry.expect("read canonical entry");
        let path = entry.path();
        if entry.file_type().expect("inspect canonical entry").is_dir() {
            found.extend(events(&path));
        } else if path
            .extension()
            .is_some_and(|extension| extension == "ndjson")
        {
            found.extend(
                fs::read_to_string(path)
                    .expect("read canonical events")
                    .lines()
                    .map(|line| serde_json::from_str(line).expect("parse canonical event")),
            );
        }
    }
    found
}

#[test]
fn masking_classes_require_explicit_declarations_before_adoption() {
    let work = evidence::directory();
    let data = work.join("data");
    let root = data.join("devin/cli");
    fs::create_dir_all(&root).expect("create owned native root");
    let database =
        rusqlite::Connection::open(root.join("sessions.db")).expect("create native store");
    database
        .execute_batch(include_str!("../devin/registry.sql"))
        .expect("load native fixture");
    let declaration = work.join("formats.json");
    let complete = json!({"assignment": null, "token": null, "entropy": null});
    for missing in complete.as_object().expect("declaration object").keys() {
        let mut invalid = complete.clone();
        invalid
            .as_object_mut()
            .expect("declaration object")
            .remove(missing);
        fs::write(
            &declaration,
            serde_json::to_vec(&invalid).expect("serialize declaration"),
        )
        .expect("write owned declaration");
        let lake = work.join(format!("lake-missing-{missing}"));
        doctor::refused(
            &work,
            &lake,
            &declaration,
            &format!("doctor-missing-{missing}"),
            missing,
        );
        let output = evidence::run(
            &work,
            &format!("refuse-missing-{missing}"),
            &[
                "--data-dir",
                lake.to_str().expect("UTF-8 Lake path"),
                "adopt",
                "--source",
                "devin",
                "--root",
                root.to_str().expect("UTF-8 native root"),
                "--json",
            ],
            &[
                ("XDG_DATA_HOME", &data),
                ("TRANSCRIPT_LAKE_SECRET_FORMATS", &declaration),
            ],
        );
        let cause = String::from_utf8_lossy(&output.stderr);
        assert!(
            !output.status.success(),
            "omitted {missing} accepted: {cause}"
        );
        assert!(cause.contains(missing), "missing field not named: {cause}");
        assert!(
            cause.contains(&declaration.display().to_string()),
            "declaration not named: {cause}"
        );
        assert!(
            events(&lake.join("events")).is_empty(),
            "invalid declaration published events"
        );
    }
    fs::write(
        &declaration,
        serde_json::to_vec(&complete).expect("serialize declaration"),
    )
    .expect("write explicit null classes");
    let lake = work.join("lake-explicit");
    let output = evidence::run(
        &work,
        "adopt-explicit-null-classes",
        &[
            "--data-dir",
            lake.to_str().expect("UTF-8 Lake path"),
            "adopt",
            "--source",
            "devin",
            "--root",
            root.to_str().expect("UTF-8 native root"),
            "--json",
        ],
        &[
            ("XDG_DATA_HOME", &data),
            ("TRANSCRIPT_LAKE_SECRET_FORMATS", &declaration),
        ],
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let expected = "the retry loop drops the last attempt";
    let persisted = events(&lake.join("events"));
    assert!(
        persisted.iter().any(|event| event["text"] == expected),
        "native message not persisted"
    );
    let (diagnostic, report, policy) =
        doctor::masking(&work, &lake, &declaration, "doctor-valid-masking-policy");
    assert!(
        diagnostic.status.success(),
        "doctor refused valid masking policy: {}",
        String::from_utf8_lossy(&diagnostic.stdout)
    );
    assert_eq!(report["healthy"], true);
    assert_eq!(policy["status"], "ok");
    assert_eq!(
        events(&lake.join("events")),
        persisted,
        "read-only doctor changed canonical events"
    );
}

/// With no file named, the masker reads the declaration shipped with the
/// product, so a stream started without the setting masks provider tokens
/// instead of refusing to take any transcript.
#[test]
fn doctor_reads_the_shipped_declaration_when_no_file_is_named() {
    let work = evidence::directory();
    let (_, _, policy) = doctor::masking(
        &work,
        &work.join("lake-shipped"),
        Path::new(""),
        "doctor-shipped-declaration",
    );
    assert_eq!(
        policy["status"], "ok",
        "the shipped declaration is not read as a valid policy: {policy}"
    );
}

#[test]
fn doctor_rejects_invalid_policies_without_creating_lake() {
    let work = evidence::directory();
    let unreadable = work.join("formats-directory");
    fs::create_dir(&unreadable).expect("create unreadable declaration directory");
    doctor::refused(
        &work,
        &work.join("lake-unreadable"),
        &unreadable,
        "doctor-unreadable",
        "cannot be read",
    );
    let declaration = work.join("formats.json");
    for (name, contents, expected) in [
        ("invalid-json", "{", "not a secret-format declaration"),
        (
            "unknown-field",
            r#"{"assignment":null,"token":null,"entropy":null,"unexpected":null}"#,
            "unknown field",
        ),
        (
            "invalid-pattern",
            r#"{"assignment":"[","token":null,"entropy":null}"#,
            "invalid assignment pattern",
        ),
    ] {
        fs::write(&declaration, contents).expect("write invalid declaration");
        doctor::refused(
            &work,
            &work.join(format!("lake-{name}")),
            &declaration,
            &format!("doctor-{name}"),
            expected,
        );
    }
}
