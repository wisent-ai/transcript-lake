//! Durable selection of the vendor transcript root this Lake follows.
//!
//! Discovery remains read-only. Adoption validates and ingests through the
//! canonical stream before this registry is changed, so a selected source is a
//! statement about accepted Lake state rather than an onboarding preference.
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::types::SUPPORTED_SOURCES;
use crate::util::{now_iso, Error, Result};

pub const SOURCE_REGISTRY_FILE: &str = "sources.json";
const SOURCE_REGISTRY_SCHEMA: u32 = 1;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AdoptedSource {
    pub id: String,
    pub runtime: String,
    pub root: PathBuf,
    #[serde(rename = "adoptedAt")]
    pub adopted_at: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SourceRegistry {
    #[serde(rename = "schemaVersion")]
    pub schema_version: u32,
    #[serde(rename = "selectedSourceId")]
    pub selected_source_id: String,
    pub sources: Vec<AdoptedSource>,
}

pub fn registry_path(data_dir: &Path) -> PathBuf {
    data_dir.join(SOURCE_REGISTRY_FILE)
}

pub fn source_identity(runtime: &str, root: &Path) -> String {
    let mut digest = Sha256::new();
    digest.update(runtime.as_bytes());
    digest.update([0]);
    digest.update(root.as_os_str().as_encoded_bytes());
    let hash = format!("{:x}", digest.finalize());
    format!("{runtime}:{}", &hash[..24])
}

pub fn read_registry(data_dir: &Path) -> Result<Option<SourceRegistry>> {
    let path = registry_path(data_dir);
    if !path.exists() {
        return Ok(None);
    }
    let registry: SourceRegistry = serde_json::from_slice(&fs::read(&path)?).map_err(|error| {
        Error(format!("invalid source registry {}: {error}", path.display()))
    })?;
    validate_registry(&registry, &path)?;
    Ok(Some(registry))
}

pub fn selected_source(data_dir: &Path) -> Result<Option<AdoptedSource>> {
    let Some(registry) = read_registry(data_dir)? else {
        return Ok(None);
    };
    Ok(registry
        .sources
        .into_iter()
        .find(|source| source.id == registry.selected_source_id))
}

pub fn persist_selection(data_dir: &Path, runtime: &str, root: &Path) -> Result<AdoptedSource> {
    let id = source_identity(runtime, root);
    let mut registry = read_registry(data_dir)?.unwrap_or(SourceRegistry {
        schema_version: SOURCE_REGISTRY_SCHEMA,
        selected_source_id: id.clone(),
        sources: Vec::new(),
    });
    let source = match registry.sources.iter().find(|source| source.id == id) {
        Some(existing) => existing.clone(),
        None => {
            let source = AdoptedSource {
                id: id.clone(),
                runtime: runtime.to_string(),
                root: root.to_path_buf(),
                adopted_at: now_iso(),
            };
            registry.sources.push(source.clone());
            source
        }
    };
    registry.selected_source_id = id;
    registry.sources.sort_by(|left, right| left.id.cmp(&right.id));
    durable_write(&registry_path(data_dir), &serde_json::to_vec_pretty(&registry)?)?;
    Ok(source)
}

fn validate_registry(registry: &SourceRegistry, path: &Path) -> Result<()> {
    if registry.schema_version != SOURCE_REGISTRY_SCHEMA {
        return Err(Error(format!(
            "unsupported source registry schema {} in {}; expected {}",
            registry.schema_version,
            path.display(),
            SOURCE_REGISTRY_SCHEMA
        )));
    }
    if registry.sources.is_empty() {
        return Err(Error(format!("source registry {} contains no sources", path.display())));
    }
    let mut ids = std::collections::BTreeSet::new();
    for source in &registry.sources {
        if !ids.insert(source.id.as_str()) {
            return Err(Error(format!(
                "source registry {} repeats source id {}",
                path.display(),
                source.id
            )));
        }
        if crate::adapters::by_name(&source.runtime).is_none()
            || !SUPPORTED_SOURCES.contains(&source.runtime.as_str())
        {
            return Err(Error(format!(
                "source registry {} names unsupported transcript runtime {}",
                path.display(),
                source.runtime
            )));
        }
        if !source.root.is_absolute()
            || source.id != source_identity(&source.runtime, &source.root)
        {
            return Err(Error(format!(
                "source registry {} carries an invalid identity for {}",
                path.display(),
                source.id
            )));
        }
    }
    if !ids.contains(registry.selected_source_id.as_str()) {
        return Err(Error(format!(
            "source registry {} selects unknown source id {}",
            path.display(),
            registry.selected_source_id
        )));
    }
    Ok(())
}

fn durable_write(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(".sources.{}.tmp", Uuid::new_v4()));
    let result = (|| -> Result<()> {
        let mut file = fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)?;
        fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}
