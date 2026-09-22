//! Operator label store: aspect/value annotations over Lake sessions. One
//! append-only NDJSON file beneath LAKE_DATA/labels; each assignment is one
//! complete line written in a single append call and fsynced before return.
//! Readers (the labels view in sql/views.sql) tolerate a torn final line, so
//! a crash mid-write loses at most the record being appended. Labels are
//! derived operator data, not masked Lake events: the events writer lease
//! deliberately does not cover this store, so labeling neither blocks nor is
//! blocked by the active stream, and deleting labels/ discards only labels.
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use regex::Regex;
use serde::Serialize;

use crate::util::{now_iso, Error, Result};

const STORE_DIR: &str = "labels";
const STORE_FILE: &str = "labels.ndjson";
const MANUAL: &str = "manual";

/// Namespaced provenance: bare manual/human/model/brama, or with a detail
/// suffix (brama:claude-opus-4.6, model:hf-distilbert-topic).
const SOURCE_PATTERN: &str = r"^(manual|human|model|brama)(:[A-Za-z0-9._/-]+)?$";

static SOURCE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(SOURCE_PATTERN).expect("source pattern"));

/// One label assignment, serialized in the field order every existing
/// labels.ndjson line uses.
#[derive(Debug, Serialize)]
pub struct LabelRecord {
    pub ts: String,
    pub session_id: String,
    pub runtime: String,
    pub aspect: String,
    pub value: String,
    pub note: Option<String>,
    pub source: String,
}

pub fn normalize_source(value: Option<&str>) -> Result<String> {
    let Some(value) = value else {
        return Ok(MANUAL.to_string());
    };
    let source = value.trim();
    if !SOURCE_RE.is_match(source) {
        return Err(Error(format!(
            "--source must match {SOURCE_PATTERN} \
             (manual, human, model, or brama, with an optional :detail suffix)"
        )));
    }
    Ok(source.to_string())
}

pub fn labels_path(data_dir: &Path) -> PathBuf {
    data_dir.join(STORE_DIR).join(STORE_FILE)
}

pub fn normalize_aspect(value: Option<&str>) -> Result<String> {
    let aspect = value.unwrap_or_default().trim().to_lowercase();
    if aspect.is_empty() {
        return Err(Error("--aspect must be a non-empty string".into()));
    }
    Ok(aspect)
}

pub fn normalize_label_value(value: Option<&str>) -> Result<String> {
    let text = value.unwrap_or_default().trim();
    if text.is_empty() {
        return Err(Error("--value must be a non-empty string".into()));
    }
    Ok(text.to_string())
}

pub fn normalize_note(value: Option<&str>) -> Option<String> {
    let text = value?.trim();
    (!text.is_empty()).then(|| text.to_string())
}

pub fn label_record(
    session_id: &str,
    runtime: &str,
    aspect: &str,
    value: &str,
    note: Option<String>,
    source: &str,
) -> LabelRecord {
    LabelRecord {
        ts: now_iso(),
        session_id: session_id.to_string(),
        runtime: runtime.to_string(),
        aspect: aspect.to_string(),
        value: value.to_string(),
        note,
        source: if source.is_empty() {
            MANUAL.to_string()
        } else {
            source.to_string()
        },
    }
}

/// Append one complete record and fsync it. Never takes the events writer
/// lease: labeling and streaming are independent stores.
pub fn append_label(data_dir: &Path, record: &LabelRecord) -> Result<()> {
    fs::create_dir_all(data_dir.join(STORE_DIR))?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(labels_path(data_dir))?;
    let mut line = serde_json::to_string(record)?;
    line.push('\n');
    file.write_all(line.as_bytes())?;
    file.sync_all()?;
    Ok(())
}
