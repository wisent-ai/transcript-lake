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

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::util::{now_iso, Error, Result};

const STORE_DIR: &str = "labels";
const STORE_FILE: &str = "labels.ndjson";

/// Who assigned a label. A source may name it more closely after a colon
/// (brama:claude-opus-4.6, model:hf-distilbert-topic).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LabelOrigin {
    Manual,
    Human,
    Model,
    Brama,
}

impl LabelOrigin {
    /// Every origin `named` accepts, in the order a refusal names them.
    const ALL: &'static [LabelOrigin] = &[
        LabelOrigin::Manual,
        LabelOrigin::Human,
        LabelOrigin::Model,
        LabelOrigin::Brama,
    ];

    /// The origin called `name`, or why no origin is.
    fn named(name: &str) -> std::result::Result<LabelOrigin, serde_json::Error> {
        serde_json::from_value(Value::String(name.to_string()))
    }

    /// The origins' stored spellings, comma separated.
    fn listed() -> String {
        Self::ALL
            .iter()
            .map(|origin| origin.name())
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// The spelling a label record stores.
    fn name(self) -> String {
        match serde_json::to_value(self) {
            Ok(Value::String(name)) => name,
            other => unreachable!("a unit variant serializes as a string, not {other:?}"),
        }
    }
}

/// A source detail names a model or a route: letters, digits, `.`, `_`,
/// `/` and `-`.
fn valid_detail(detail: &str) -> bool {
    !detail.is_empty()
        && detail.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '/' | '-')
        })
}

/// One label assignment, serialized in the field order every existing
/// labels.ndjson line uses. A withdrawal is the same record with no
/// `value`: appended like an assignment, it is the latest row for its
/// session and aspect, and the reads that keep the latest row then see no
/// assignment in force — the log stays append-only and the history whole.
#[derive(Debug, Serialize)]
pub struct LabelRecord {
    pub ts: String,
    pub session_id: String,
    pub runtime: String,
    pub aspect: String,
    pub value: Option<String>,
    pub note: Option<String>,
    pub source: String,
}

/// The `--source` of a label: an origin, optionally `:detail`. Without one
/// the label was set by hand.
pub fn normalize_source(value: Option<&str>) -> Result<String> {
    let Some(value) = value else {
        return Ok(LabelOrigin::Manual.name());
    };
    let source = value.trim();
    let (origin, detail) = match source.split_once(':') {
        Some((origin, detail)) => (origin, Some(detail)),
        None => (source, None),
    };
    if LabelOrigin::named(origin).is_err() || detail.is_some_and(|detail| !valid_detail(detail)) {
        return Err(Error(format!(
            "--source must be one of {}, with an optional :detail of \
             letters, digits, '.', '_', '/' or '-'; got {source:?}",
            LabelOrigin::listed()
        )));
    }
    Ok(source.to_string())
}

pub fn labels_path(data_dir: &Path) -> PathBuf {
    data_dir.join(STORE_DIR).join(STORE_FILE)
}

/// The `--aspect` of a label, lowercased; absent or blank is refused.
pub fn normalize_aspect(value: Option<&str>) -> Result<String> {
    match value.map(|value| value.trim().to_lowercase()) {
        Some(aspect) if !aspect.is_empty() => Ok(aspect),
        _ => Err(Error("--aspect must be a non-empty string".into())),
    }
}

/// The `--value` of a label, trimmed; absent or blank is refused.
pub fn normalize_label_value(value: Option<&str>) -> Result<String> {
    match value.map(str::trim) {
        Some(text) if !text.is_empty() => Ok(text.to_string()),
        _ => Err(Error("--value must be a non-empty string".into())),
    }
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
        value: Some(value.to_string()),
        note,
        source: source.to_string(),
    }
}

/// The record that withdraws a session's label for one aspect.
pub fn withdrawal_record(
    session_id: &str,
    runtime: &str,
    aspect: &str,
    note: Option<String>,
    source: &str,
) -> LabelRecord {
    LabelRecord {
        value: None,
        ..label_record(session_id, runtime, aspect, "", note, source)
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
