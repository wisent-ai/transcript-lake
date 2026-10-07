//! The model this command runs on: Ster, the product that owns local model
//! inference, run over a checkpoint directory assembled here from the
//! published GGUF and its base model's config and tokenizer, each checked
//! against what it is published under.

use std::env;
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::UNIX_EPOCH;

use sha2::{Digest, Sha256};

use crate::util::{find_on_path, home_dir, Error, Result};

use super::{
    ModelValidationStamp, MODEL_NAME, MODEL_REVISION, MODEL_SHA256, PROMPT_NAME, PROMPT_SHA256,
    REPOSITORY,
};

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

/// The base model the goal model was fine-tuned from, as its published card
/// at the pinned revision declares it (`cardData.base_model`).
fn base_model(data_dir: &Path) -> Result<String> {
    let card = artifact_dir(data_dir).join("model-card.json");
    if !card.is_file() {
        download(
            &format!("https://huggingface.co/api/models/{REPOSITORY}/revision/{MODEL_REVISION}"),
            &card,
        )?;
    }
    let read: serde_json::Value = serde_json::from_slice(&fs::read(&card)?).map_err(|error| {
        Error(format!(
            "model card {} is not JSON: {error}",
            card.display()
        ))
    })?;
    let declared = &read["cardData"]["base_model"];
    let base = declared.as_str().or_else(|| {
        declared
            .as_array()
            .and_then(|bases| bases.iter().find_map(serde_json::Value::as_str))
    });
    base.map(str::to_owned).ok_or_else(|| {
        Error(format!(
            "{REPOSITORY}@{MODEL_REVISION} declares no base_model in its card ({}), so Ster has no config or tokenizer to run it with",
            card.display()
        ))
    })
}

/// The checkpoint directory Ster loads: the validated GGUF linked beside the
/// base model's config, tokenizer and tokenizer config, fetched once at the
/// base repository's revision of that moment, which `base-revision` records.
pub(super) fn resolve_checkpoint(data_dir: &Path, model: &Path) -> Result<PathBuf> {
    let directory = artifact_dir(data_dir).join("checkpoint");
    fs::create_dir_all(&directory)?;
    let linked = directory.join(MODEL_NAME);
    if fs::read_link(&linked).ok().as_deref() != Some(model) {
        let _ = fs::remove_file(&linked);
        std::os::unix::fs::symlink(model, &linked)?;
    }
    let base = base_model(data_dir)?;
    let recorded = directory.join("base-revision");
    let revision = match fs::read_to_string(&recorded) {
        Ok(revision) => revision.trim().to_owned(),
        Err(_) => {
            let info = artifact_dir(data_dir).join("base-model.json");
            download(&format!("https://huggingface.co/api/models/{base}"), &info)?;
            let read: serde_json::Value =
                serde_json::from_slice(&fs::read(&info)?).map_err(|error| {
                    Error(format!(
                        "base model record {} is not JSON: {error}",
                        info.display()
                    ))
                })?;
            let sha = read["sha"]
                .as_str()
                .ok_or_else(|| {
                    Error(format!(
                        "base model {base} answered no revision ({})",
                        info.display()
                    ))
                })?
                .to_owned();
            fs::write(&recorded, &sha)?;
            sha
        }
    };
    for file in ["config.json", "tokenizer.json", "tokenizer_config.json"] {
        let path = directory.join(file);
        if !path.is_file() {
            download(
                &format!("https://huggingface.co/{base}/resolve/{revision}/{file}"),
                &path,
            )?;
        }
    }
    Ok(directory)
}

/// The context the base model declares (`max_position_embeddings`): the goal
/// is generated until the model ends it or the context is full, as before.
pub(super) fn declared_context(checkpoint: &Path) -> Result<u64> {
    let config = checkpoint.join("config.json");
    let read: serde_json::Value = serde_json::from_slice(&fs::read(&config)?)
        .map_err(|error| Error(format!("{} is not JSON: {error}", config.display())))?;
    read["max_position_embeddings"].as_u64().ok_or_else(|| {
        Error(format!(
            "{} declares no max_position_embeddings",
            config.display()
        ))
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
        .args(["--fail", "--location", "--output"])
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
    // The artifact is pinned by its SHA-256; its size is whatever that digest
    // covers, so no byte count is compiled in.
    if !metadata.is_file() {
        return Err(Error(format!(
            "model {} is not a regular file",
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
    io::copy(&mut file, &mut digest)?;
    let actual = format!("{:x}", digest.finalize());
    if actual != expected {
        return Err(Error(format!("{name} {} failed SHA-256", path.display())));
    }
    Ok(())
}
