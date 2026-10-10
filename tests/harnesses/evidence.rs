use serde_json::json;
use std::{fs, path::Path, process::{Command, Output}};

pub fn run(root: &Path, name: &str, args: &[&str]) -> Output {
    let output = Command::new(env!("CARGO_BIN_EXE_transcript-lake"))
        .args(args).current_dir(root).output().expect("invoke real Lake CLI");
    fs::write(root.join(format!("{name}.json")), serde_json::to_vec_pretty(&json!({
        "binary": env!("CARGO_BIN_EXE_transcript-lake"), "args": args,
        "exitCode": output.status.code(), "success": output.status.success(),
        "stdout": String::from_utf8_lossy(&output.stdout),
        "stderr": String::from_utf8_lossy(&output.stderr),
    })).expect("serialize command evidence")).expect("retain command evidence");
    output
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
        let output = Command::new("git").args(args)
            .current_dir(env!("CARGO_MANIFEST_DIR")).output().expect("record source identity");
        fs::write(root.join(name), &output.stdout).expect("retain source identity");
        assert!(output.status.success(), "git: {}", String::from_utf8_lossy(&output.stderr));
    }
    let binary = fs::read(env!("CARGO_BIN_EXE_transcript-lake")).expect("read tested binary");
    fs::write(root.join("binary.sha256"), format!("{:x}", Sha256::digest(binary)))
        .expect("retain binary identity");
    eprintln!("retained journey: {}", root.display());
    root
}
