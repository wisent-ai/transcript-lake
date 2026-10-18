//! The checkpoint directory Ster loads: the validated GGUF linked beside the
//! config and tokenizer of the base model it was fine-tuned from.
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use crate::util::{Error, Result};

use super::super::{MODEL_NAME, MODEL_REVISION, REPOSITORY};
use super::{artifact_dir, download};

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
pub(crate) fn resolve_checkpoint(data_dir: &Path, model: &Path) -> Result<PathBuf> {
    let directory = artifact_dir(data_dir).join("checkpoint");
    fs::create_dir_all(&directory)?;
    let linked = directory.join(MODEL_NAME);
    // Nothing linked yet, or something there that is not a link, is replaced
    // like a link to another file; a link that cannot be read is the error.
    let current = match fs::read_link(&linked) {
        Ok(target) => Some(target),
        Err(error) if matches!(error.kind(), ErrorKind::NotFound | ErrorKind::InvalidInput) => None,
        Err(error) => {
            return Err(Error(format!(
                "cannot read the checkpoint link {}: {error}",
                linked.display()
            )))
        }
    };
    if current.as_deref() != Some(model) {
        match fs::remove_file(&linked) {
            Ok(()) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(error) => {
                return Err(Error(format!(
                    "cannot replace the checkpoint link {}: {error}",
                    linked.display()
                )))
            }
        }
        std::os::unix::fs::symlink(model, &linked)?;
    }
    let base = base_model(data_dir)?;
    let recorded = directory.join("base-revision");
    // The base revision is fetched once and recorded; a record that exists and
    // cannot be read is the error, not a reason to fetch another revision.
    let revision = match fs::read_to_string(&recorded) {
        Ok(revision) => revision.trim().to_owned(),
        Err(error) if error.kind() == ErrorKind::NotFound => {
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
            crate::lake::durable::publish(&recorded, sha.as_bytes())?;
            sha
        }
        Err(error) => {
            return Err(Error(format!(
                "cannot read the recorded base revision {}: {error}",
                recorded.display()
            )))
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
pub(crate) fn declared_context(checkpoint: &Path) -> Result<u64> {
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
