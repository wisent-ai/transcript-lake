//! The model this command runs on: Ster, the product that owns local model
//! inference, run over a checkpoint directory assembled here from the
//! published GGUF and its base model's config and tokenizer, each checked
//! against what it is published under.

use std::env;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::UNIX_EPOCH;

use sha2::{Digest, Sha256};

use crate::util::{find_on_path, Error, Result};

use super::{
    ModelValidationStamp, MODEL_NAME, MODEL_REVISION, MODEL_SHA256, PROMPT_NAME, PROMPT_SHA256,
    REPOSITORY,
};

mod checkpoint;

pub(super) use checkpoint::{declared_context, resolve_checkpoint};

/// Ster's CLI: `TRANSCRIPT_LAKE_STER` when it names a file, else `ster` on
/// PATH. Ster runs the GGUF on its own decoder (a local directory holding one
/// `.gguf` beside the base model's `config.json` and tokenizer).
pub(super) fn resolve_runtime() -> Result<PathBuf> {
    let key = "TRANSCRIPT_LAKE_STER";
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
    find_on_path("ster").ok_or_else(|| {
        Error("local goal model runs on Ster: install ster on PATH or name it with TRANSCRIPT_LAKE_STER".into())
    })
}

/// The goal model: the file `TRANSCRIPT_LAKE_GOAL_MODEL` names, checked, or
/// Transcript Lake's own copy under its data directory, fetched from the
/// published revision when it holds none or holds one that fails its check.
/// No other product's cache or storage is read.
pub(super) fn resolve_model(data_dir: &Path) -> Result<PathBuf> {
    if let Some(path) = env::var_os("TRANSCRIPT_LAKE_GOAL_MODEL")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
    {
        validate_model(data_dir, &path)?;
        return Ok(path);
    }
    let cached = artifact_dir(data_dir).join(MODEL_NAME);
    owned_copy(&cached, MODEL_NAME, |path| validate_model(data_dir, path))
}

/// The system prompt published beside the model, kept beside Transcript
/// Lake's own copy of it.
pub(super) fn resolve_prompt(data_dir: &Path) -> Result<PathBuf> {
    let cached = artifact_dir(data_dir).join(PROMPT_NAME);
    owned_copy(&cached, PROMPT_NAME, |path| {
        validate_digest(path, PROMPT_SHA256, "system prompt")
    })
}

/// `cached` when `check` accepts it; otherwise the published `name` fetched
/// into it and checked. A copy that fails its check is said on stderr before
/// it is fetched again, so a replaced artifact is never silent.
fn owned_copy(cached: &Path, name: &str, check: impl Fn(&Path) -> Result<()>) -> Result<PathBuf> {
    if cached.exists() {
        match check(cached) {
            Ok(()) => return Ok(cached.to_path_buf()),
            Err(Error(why)) => eprintln!("{why}; fetching {name} again"),
        }
    }
    download(
        &format!("https://huggingface.co/{REPOSITORY}/resolve/{MODEL_REVISION}/{name}"),
        cached,
    )?;
    check(cached)?;
    Ok(cached.to_path_buf())
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
    let status = stado_wait::status(
        Command::new("curl")
            .args(["--fail", "--location", "--output"])
            .arg(&temporary)
            .arg(url),
    )
    .map_err(|error| Error(format!("failed to start curl for model artifact: {error}")))?;
    if !status.success() {
        // curl may leave nothing or a partial file; a partial file that cannot
        // be removed is named with the failure.
        let left = match fs::remove_file(&temporary) {
            Ok(()) => String::new(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => format!("; {} was left behind: {error}", temporary.display()),
        };
        return Err(Error(format!(
            "model artifact download from {url} failed with status {status}{left}"
        )));
    }
    if let Err(error) = fs::rename(&temporary, destination) {
        let left = match fs::remove_file(&temporary) {
            Ok(()) => String::new(),
            Err(cleanup) if cleanup.kind() == io::ErrorKind::NotFound => String::new(),
            Err(cleanup) => format!("; {} was left behind: {cleanup}", temporary.display()),
        };
        return Err(Error(format!(
            "could not publish model artifact {} as {}: {error}{left}",
            temporary.display(),
            destination.display()
        )));
    }
    Ok(())
}

pub(super) fn validate_model(data_dir: &Path, path: &Path) -> Result<()> {
    let metadata = fs::metadata(path)
        .map_err(|error| Error(format!("model {} is unavailable: {error}", path.display())))?;
    // The artifact is pinned by its SHA-256; its size is whatever that digest
    // covers, so no byte count is compiled in.
    if !metadata.is_file() {
        return Err(Error(format!(
            "model {} is not a regular file",
            path.display()
        )));
    }
    // The stamp lets a later run skip hashing an unchanged file; a file whose
    // modification time cannot be read has no stamp to trust, so it is refused.
    let modified = metadata
        .modified()
        .map_err(|error| {
            Error(format!(
                "model {} has no modification time to stamp its validation with: {error}",
                path.display()
            ))
        })?
        .duration_since(UNIX_EPOCH)
        .map_err(|error| {
            Error(format!(
                "model {} was modified before the epoch: {error}",
                path.display()
            ))
        })?;
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
    crate::lake::durable::publish(&stamp_path, &serde_json::to_vec(&expected)?)
}

pub(super) fn validate_digest(path: &Path, expected: &str, name: &str) -> Result<()> {
    let mut file = File::open(path)
        .map_err(|error| Error(format!("{name} {} is unavailable: {error}", path.display())))?;
    let mut digest = Sha256::new();
    io::copy(&mut file, &mut digest)?;
    let actual = format!("{:x}", digest.finalize());
    if actual != expected {
        return Err(Error(format!("{name} {} failed SHA-256", path.display())));
    }
    Ok(())
}
