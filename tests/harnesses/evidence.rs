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
    let mut command = Command::new(env!("CARGO_BIN_EXE_transcript-lake"));
    command
        .args(args)
        .envs(environment.iter().copied())
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(fs::File::create(&stdout_path).expect("create live stdout evidence"))
        .stderr(fs::File::create(&stderr_path).expect("create live stderr evidence"));
    bound_by_file_permissions(&mut command);
    let spawned = command.spawn();
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

/// A journey run by root on Linux (a Stado run on a build host is) gives the
/// Lake root's override of file permissions, so a directory or file the
/// journey made unreadable is read anyway and the refusal it checks for never
/// happens. The child loses CAP_DAC_OVERRIDE and CAP_DAC_READ_SEARCH from its
/// bounding set before it executes, so it reads the journey's files as their
/// owner would: the permission bits decide. Any other user has neither
/// capability and is left as it is.
#[cfg(target_os = "linux")]
fn bound_by_file_permissions(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    // linux/capability.h; the libc crate does not declare capability numbers.
    const CAP_DAC_OVERRIDE: libc::c_ulong = 1;
    const CAP_DAC_READ_SEARCH: libc::c_ulong = 2;
    // geteuid has no preconditions and cannot fail.
    if unsafe { libc::geteuid() } != 0 {
        return;
    }
    // The closure runs in the forked child before exec and calls only
    // prctl, which is async-signal-safe; a capability it cannot drop fails
    // the spawn with prctl's error instead of running the journey as root.
    unsafe {
        command.pre_exec(|| {
            for capability in [CAP_DAC_OVERRIDE, CAP_DAC_READ_SEARCH] {
                if libc::prctl(libc::PR_CAPBSET_DROP, capability, 0, 0, 0) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
}

/// Outside Linux a journey does not run as root, and a process's file access
/// is the permission bits'.
#[cfg(not(target_os = "linux"))]
fn bound_by_file_permissions(_command: &mut Command) {}

/// One command a journey ran and that succeeded, as a criterion report lists
/// it.
pub fn command(args: &[&str], output: &Output) -> Value {
    json!({
        "executable": env!("CARGO_BIN_EXE_transcript-lake"),
        "args": args,
        "exit": output.status.code(),
        "stderr": String::from_utf8_lossy(&output.stderr),
    })
}

fn raw_git(args: &[&str]) -> Vec<u8> {
    let output = match Command::new("git")
        .args(args)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
    {
        Ok(output) => output,
        Err(error) => panic!("run git {args:?}: {error}"),
    };
    assert!(
        output.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn git(args: &[&str]) -> String {
    String::from_utf8_lossy(&raw_git(args)).trim().to_owned()
}

/// The commit Stado exported this tree from. `stado product cargo test
/// --revision` runs a journey in an exported tree with no Git checkout and
/// states the commit it exported (`WISENT_SOURCE_COMMIT`), as a release
/// worker does: that tree is the commit, with nothing uncommitted. Absent,
/// the journey runs in the checkout and reads its own HEAD and changes.
fn exported() -> Option<String> {
    match std::env::var("WISENT_SOURCE_COMMIT") {
        Ok(commit) => Some(commit),
        Err(std::env::VarError::NotPresent) => None,
        Err(error) => panic!("WISENT_SOURCE_COMMIT is set but cannot be read: {error}"),
    }
}

fn named(variable: &str) -> String {
    match std::env::var(variable) {
        Ok(value) => value,
        Err(error) => panic!(
            "{variable} names what the report answers and TRANSCRIPT_LAKE_JOURNEY_REPORT asks for a report: {error}"
        ),
    }
}

/// With `TRANSCRIPT_LAKE_JOURNEY_REPORT=<file>` a passed journey writes the
/// criterion report Oko reads before it records a defect repaired: the
/// committed source revision it ran (a tree with uncommitted changes is
/// refused), the digests of the tested binary and of the journey's source,
/// every command that succeeded with its arguments, exit status and stderr,
/// and the final state it observed. `TRANSCRIPT_LAKE_JOURNEY_SUBJECT` names
/// the register entry and `TRANSCRIPT_LAKE_JOURNEY_CRITERION` the criterion
/// the report answers; each is refused by name when a report is asked for
/// without it. Without the report variable nothing is written.
pub fn criterion_report(journey_source: &[u8], commands: Vec<Value>, observed: Value) {
    use sha2::{Digest, Sha256};
    let Ok(path) = std::env::var("TRANSCRIPT_LAKE_JOURNEY_REPORT") else {
        return;
    };
    let revision = match exported() {
        Some(commit) => commit,
        None => {
            let changed = git(&["status", "--porcelain"]);
            assert!(
                changed.is_empty(),
                "a report names a committed source; the tree has changes:\n{changed}"
            );
            git(&["rev-parse", "HEAD"])
        }
    };
    let binary = fs::read(env!("CARGO_BIN_EXE_transcript-lake")).expect("read tested binary");
    let document = json!({
        "defectId": named("TRANSCRIPT_LAKE_JOURNEY_SUBJECT"),
        "criterion": named("TRANSCRIPT_LAKE_JOURNEY_CRITERION"),
        "sourceRevision": revision,
        "result": "passed",
        "binarySha256": format!("{:x}", Sha256::digest(binary)),
        "testSha256": format!("{:x}", Sha256::digest(journey_source)),
        "commands": commands,
        "observedFinalState": observed,
    });
    let bytes = serde_json::to_vec_pretty(&document).expect("serialize the report");
    if let Err(error) = fs::write(&path, bytes) {
        panic!("write the report {path}: {error}");
    }
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
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(".build/journeys")
        .join(format!("harness-ingestion-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&root).expect("create retained journey directory");
    let (revision, patch) = match exported() {
        Some(commit) => (commit.into_bytes(), Vec::new()),
        None => (
            raw_git(&["rev-parse", "HEAD"]),
            raw_git(&["diff", "--binary", "HEAD"]),
        ),
    };
    for (name, bytes) in [("source-revision", revision), ("source-patch", patch)] {
        if let Err(error) = fs::write(root.join(name), bytes) {
            panic!("retain source identity {name}: {error}");
        }
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
