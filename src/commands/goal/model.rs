//! The local model this command runs on: where the runtime, the weights and
//! the system prompt are found, how each is checked against the digest it is
//! published under, and the temporary input file one inference is given.

use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

use crate::util::{find_on_path, home_dir, Error, Result};

use super::{ModelValidationStamp, MODEL_BYTES, MODEL_NAME, MODEL_REVISION, MODEL_SHA256, PROMPT_NAME, PROMPT_SHA256, REPOSITORY};


pub(super) fn resolve_runtime() -> Result<PathBuf> {
    for key in ["TRANSCRIPT_LAKE_GOAL_LLAMA_CLI", "JEDEN_GOAL_LLAMA_CLI"] {
        if let Some(path) = env::var_os(key)
            .map(PathBuf::from)
            .filter(|path| !path.as_os_str().is_empty())
        {
            if path.is_file() {
                return Ok(path);
            }
            return Err(Error(format!(
                "{key} does not name a file: {}",
                path.display()
            )));
        }
    }
    find_on_path("llama-cli").ok_or_else(|| {
        Error(
            "local goal model requires llama-cli on PATH or TRANSCRIPT_LAKE_GOAL_LLAMA_CLI".into(),
        )
    })
}

pub(super) fn resolve_model(data_dir: &Path) -> Result<PathBuf> {
    if let Some(path) = env::var_os("TRANSCRIPT_LAKE_GOAL_MODEL")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
    {
        validate_model(data_dir, &path)?;
        return Ok(path);
    }
    let shared = home_dir()
        .join("Library/Caches/ai.wisent.jeden.desktop/goal-model")
        .join(MODEL_NAME);
    if validate_model(data_dir, &shared).is_ok() {
        return Ok(shared);
    }
    let cached = artifact_dir(data_dir).join(MODEL_NAME);
    if validate_model(data_dir, &cached).is_ok() {
        return Ok(cached);
    }
    download(
        &format!("https://huggingface.co/{REPOSITORY}/resolve/{MODEL_REVISION}/{MODEL_NAME}"),
        &cached,
    )?;
    validate_model(data_dir, &cached)?;
    Ok(cached)
}

pub(super) fn resolve_prompt(data_dir: &Path) -> Result<PathBuf> {
    let stado = home_dir()
        .join(".stado/local-storage/ecosystem/releases/jeden-desktop/models/goal-qwen3-4b")
        .join(MODEL_SHA256)
        .join(PROMPT_NAME);
    if validate_digest(&stado, PROMPT_SHA256, "system prompt").is_ok() {
        return Ok(stado);
    }
    let cached = artifact_dir(data_dir).join(PROMPT_NAME);
    if validate_digest(&cached, PROMPT_SHA256, "system prompt").is_ok() {
        return Ok(cached);
    }
    download(
        &format!("https://huggingface.co/{REPOSITORY}/resolve/{MODEL_REVISION}/{PROMPT_NAME}"),
        &cached,
    )?;
    validate_digest(&cached, PROMPT_SHA256, "system prompt")?;
    Ok(cached)
}

pub(super) fn artifact_dir(data_dir: &Path) -> PathBuf {
    data_dir
        .join("models/jeden-goal-qwen3-4b")
        .join(MODEL_SHA256)
}

pub(super) fn download(url: &str, destination: &Path) -> Result<()> {
    let parent = destination.parent().ok_or_else(|| {
        Error(format!(
            "invalid model destination: {}",
            destination.display()
        ))
    })?;
    fs::create_dir_all(parent)?;
    let temporary = destination.with_extension(format!("download-{}", std::process::id()));
    let status = Command::new("curl")
        .args(["--fail", "--location", "--retry", "3", "--output"])
        .arg(&temporary)
        .arg(url)
        .status()
        .map_err(|error| Error(format!("failed to start curl for model artifact: {error}")))?;
    if !status.success() {
        let _ = fs::remove_file(&temporary);
        return Err(Error(format!(
            "model artifact download failed with status {status}"
        )));
    }
    fs::rename(&temporary, destination)?;
    Ok(())
}

pub(super) fn validate_model(data_dir: &Path, path: &Path) -> Result<()> {
    let metadata = fs::metadata(path)
        .map_err(|error| Error(format!("model {} is unavailable: {error}", path.display())))?;
    if !metadata.is_file() || metadata.len() != MODEL_BYTES {
        return Err(Error(format!(
            "model {} has the wrong size",
            path.display()
        )));
    }
    let modified = metadata
        .modified()
        .unwrap_or(UNIX_EPOCH)
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let expected = ModelValidationStamp {
        path: path.to_string_lossy().to_string(),
        bytes: metadata.len(),
        modified_secs: modified.as_secs(),
        modified_nanos: modified.subsec_nanos(),
        sha256: MODEL_SHA256.to_string(),
    };
    let stamp_path = artifact_dir(data_dir).join("model-validation.json");
    if let Ok(bytes) = fs::read(&stamp_path) {
        if let Ok(stored) = serde_json::from_slice::<ModelValidationStamp>(&bytes) {
            if stored.path == expected.path
                && stored.bytes == expected.bytes
                && stored.modified_secs == expected.modified_secs
                && stored.modified_nanos == expected.modified_nanos
                && stored.sha256 == expected.sha256
            {
                return Ok(());
            }
        }
    }
    validate_digest(path, MODEL_SHA256, "model")?;
    fs::create_dir_all(artifact_dir(data_dir))?;
    let temporary = stamp_path.with_extension(format!("json-{}", std::process::id()));
    fs::write(&temporary, serde_json::to_vec(&expected)?)?;
    fs::rename(temporary, stamp_path)?;
    Ok(())
}

pub(super) fn validate_digest(path: &Path, expected: &str, name: &str) -> Result<()> {
    let mut file = File::open(path)
        .map_err(|error| Error(format!("{name} {} is unavailable: {error}", path.display())))?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 4 * 1024 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    let actual = format!("{:x}", digest.finalize());
    if actual != expected {
        return Err(Error(format!("{name} {} failed SHA-256", path.display())));
    }
    Ok(())
}

pub(super) fn temporary_input(data_dir: &Path, text: &str) -> Result<PathBuf> {
    let directory = data_dir.join("tmp");
    fs::create_dir_all(&directory)?;
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let path = directory.join(format!("goal-input-{}-{nonce}.txt", std::process::id()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)?;
    write!(file, "<user>{}</user>", text.replace('\0', ""))?;
    file.sync_all()?;
    Ok(path)
}
