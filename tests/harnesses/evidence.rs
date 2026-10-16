use serde_json::{json, Value};
use std::{
    fs,
    path::Path,
    process::{Command, Output, Stdio},
};

pub fn run(root: &Path, name: &str, args: &[&str], environment: &[(&str, &Path)]) -> Output {
    let stdout_path = root.join(format!("{name}.stdout"));
    let stderr_path = root.join(format!("{name}.stderr"));
    let mut record = json!({
        "binary": env!("CARGO_BIN_EXE_transcript-lake"), "args": args,
        "cwd": root, "environment": environment, "state": "starting",
        "startedAt": chrono::Utc::now().to_rfc3339(),
        "stdoutPath": stdout_path, "stderrPath": stderr_path,
    });
    let retain = |record: &serde_json::Value| {
        fs::write(
            root.join(format!("{name}.json")),
            serde_json::to_vec_pretty(record).expect("serialize command evidence"),
        )
        .expect("retain command evidence");
    };
    retain(&record);
    eprintln!(
        "running {name}; live output: {} and {}",
        stdout_path.display(),
        stderr_path.display()
    );
    let spawned = Command::new(env!("CARGO_BIN_EXE_transcript-lake"))
        .args(args)
        .envs(environment.iter().copied())
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(fs::File::create(&stdout_path).expect("create live stdout evidence"))
        .stderr(fs::File::create(&stderr_path).expect("create live stderr evidence"))
        .spawn();
    let mut child = match spawned {
        Ok(child) => child,
        Err(error) => {
            record["state"] = json!("failed");
            record["operation"] = json!("spawn");
            record["error"] = json!(error.to_string());
            retain(&record);
            panic!("invoke real Lake CLI: {error}");
        }
    };
    record["state"] = json!("running");
    record["pid"] = json!(child.id());
    retain(&record);
    let status = match child.wait() {
        Ok(status) => status,
        Err(error) => {
            record["state"] = json!("failed");
            record["operation"] = json!("wait");
            record["error"] = json!(error.to_string());
            retain(&record);
            panic!("wait for real Lake CLI: {error}");
        }
    };
    record["state"] = json!("exited");
    record["finishedAt"] = json!(chrono::Utc::now().to_rfc3339());
    record["exitCode"] = json!(status.code());
    record["success"] = json!(status.success());
    retain(&record);
    let output = Output {
        status,
        stdout: fs::read(&stdout_path).expect("read retained stdout"),
        stderr: fs::read(&stderr_path).expect("read retained stderr"),
    };
    record["stdout"] = json!(String::from_utf8_lossy(&output.stdout));
    record["stderr"] = json!(String::from_utf8_lossy(&output.stderr));
    retain(&record);
    output
}

pub fn show_retains_canonical_extra(
    root: &Path,
    lake: &str,
    session: &str,
    user_text: &str,
    canonical: &[Value],
    environment: &[(&str, &Path)],
) {
    let source = canonical
        .iter()
        .find(|event| {
            event["session_id"] == session
                && event["event_type"] == "user"
                && event["text"] == user_text
        })
        .expect("canonical user event");
    assert!(
        source["extra"].is_object(),
        "canonical user metadata is not an object: {source}"
    );
    let output = run(
        root,
        "show-user",
        &[
            "--data-dir",
            lake,
            "show",
            session,
            "--include",
            "user",
            "--json",
        ],
        environment,
    );
    assert!(
        output.status.success(),
        "show user events: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let shown: Value = serde_json::from_slice(&output.stdout).expect("show JSON");
    let turn = shown["events"]
        .as_array()
        .expect("shown events")
        .iter()
        .find(|event| event["text"] == user_text)
        .expect("shown user turn");
    assert_eq!(
        turn["extra"], source["extra"],
        "show lost canonical attribution: {turn}"
    );
}

pub fn directory() -> std::path::PathBuf {
    use sha2::{Digest, Sha256};
    let root = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("harness-ingestion-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&root).expect("create retained journey directory");
    for (name, args) in [
        ("source-revision", vec!["rev-parse", "HEAD"]),
        ("source-patch", vec!["diff", "--binary", "HEAD"]),
    ] {
        let output = Command::new("git")
            .args(args)
            .current_dir(env!("CARGO_MANIFEST_DIR"))
            .output()
            .expect("record source identity");
        fs::write(root.join(name), &output.stdout).expect("retain source identity");
        assert!(
            output.status.success(),
            "git: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let binary = fs::read(env!("CARGO_BIN_EXE_transcript-lake")).expect("read tested binary");
    fs::write(
        root.join("binary.sha256"),
        format!("{:x}", Sha256::digest(binary)),
    )
    .expect("retain binary identity");
    eprintln!("retained journey: {}", root.display());
    root
}

/// Writes a secret formats declaration that recognises no class, so a
/// journey's adopted words reach the Lake as written, and returns the path
/// `TRANSCRIPT_LAKE_SECRET_FORMATS` names.
pub fn no_secret_classes(root: &Path) -> std::path::PathBuf {
    let declaration = root.join("formats.json");
    fs::write(
        &declaration,
        json!({"assignment": null, "token": null, "entropy": null}).to_string(),
    )
    .expect("write the secret formats declaration");
    declaration
}
