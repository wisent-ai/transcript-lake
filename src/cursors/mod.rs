//! Cursor store for mutable byte streams and immutable closed segments, plus
//! the single-writer state lease. Legacy records retain their numeric coercion
//! contract; tagged segment commits are stored as structured records and never
//! projected onto byte-offset fields. Writes are durable: unique temp, file
//! sync, rename, then parent sync.
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::util::{mtime_ms, Error, Result};

mod lease;
mod source;
pub use lease::open_writer_lease;
use lease::{acquire_lock, release_lock, sync_directory};
pub use source::SourceCheckpoint;

const SEGMENT_KIND: &str = "closed-segment";

/// A complete-line resume point with proof of the consumed source prefix.
#[derive(Debug, Clone, Copy)]
pub struct ByteCursor {
    pub mtime_ms: f64,
    pub size: u64,
    pub offset: u64,
    pub source: Option<SourceCheckpoint>,
}

impl ByteCursor {
    pub fn is_current(&self, meta: &fs::Metadata) -> bool {
        self.source.is_some_and(|source| source.matches(meta))
            && self.mtime_ms == mtime_ms(meta)
            && self.size == meta.len()
            && self.offset >= meta.len()
    }
}

/// What the store holds for one source path.
#[derive(Debug, Clone)]
pub enum CursorRecord {
    Bytes(ByteCursor),
    /// An immutable closed segment commit, passed through untouched.
    Segment(Value),
}

fn coerce(record: &Value) -> Result<ByteCursor> {
    let number = |key: &str| record.get(key).and_then(Value::as_f64);
    let (Some(mtime_ms), Some(size), Some(offset)) =
        (number("mtimeMs"), number("size"), number("offset"))
    else {
        return Err(Error("cursor record contains invalid numeric state".into()));
    };
    if !mtime_ms.is_finite()
        || !size.is_finite()
        || !offset.is_finite()
        || size < 0.0
        || offset < 0.0
    {
        return Err(Error("cursor record contains invalid numeric state".into()));
    }
    Ok(ByteCursor {
        mtime_ms,
        size: size as u64,
        offset: offset as u64,
        source: Option::<SourceCheckpoint>::deserialize(
            record.get("source").unwrap_or(&Value::Null),
        )?,
    })
}

/// Cursor loss can replay already-persisted evidence, so unreadable state is a
/// hard failure. Recovery uses a separate empty LAKE_DATA root, never a silent
/// fallback that appends a second copy to existing partitions.
fn read_store(path: &Path) -> Result<Map<String, Value>> {
    if !path.exists() {
        return Ok(Map::new());
    }
    let raw = fs::read_to_string(path).map_err(|error| {
        Error(format!(
            "cursor store is unreadable; preserve it and recover into an empty LAKE_DATA: {error}"
        ))
    })?;
    let parsed: Value = serde_json::from_str(&raw).map_err(|error| {
        Error(format!(
            "cursor store is corrupt; preserve it and recover into an empty LAKE_DATA: {error}"
        ))
    })?;
    match parsed {
        Value::Object(map) => Ok(map),
        _ => Err(Error(
            "cursor store must be a JSON object; preserve it and recover into an empty LAKE_DATA"
                .into(),
        )),
    }
}

/// The resume-point store: read at open, merged under its own lock on flush.
pub struct Cursors {
    data_dir: PathBuf,
    file_path: PathBuf,
    lock_path: PathBuf,
    store: Map<String, Value>,
    pending: Map<String, Value>,
    dirty: bool,
}

impl Cursors {
    pub fn open(data_dir: &Path) -> Result<Self> {
        let file_path = data_dir.join("cursors.json");
        let store = read_store(&file_path)?;
        Ok(Self {
            data_dir: data_dir.to_path_buf(),
            file_path,
            lock_path: data_dir.join("cursors.lock"),
            store,
            pending: Map::new(),
            dirty: false,
        })
    }

    pub fn get(&self, file: &str) -> Result<Option<CursorRecord>> {
        let Some(record) = self.store.get(file) else {
            return Ok(None);
        };
        if !record.is_object() {
            return Ok(None);
        }
        if record.get("kind").and_then(Value::as_str) == Some(SEGMENT_KIND) {
            return Ok(Some(CursorRecord::Segment(record.clone())));
        }
        Ok(Some(CursorRecord::Bytes(coerce(record)?)))
    }

    pub fn set_bytes(&mut self, file: &str, cursor: ByteCursor) {
        let value = json!({
            "mtimeMs": cursor.mtime_ms,
            "size": cursor.size,
            "offset": cursor.offset,
            "source": cursor.source,
        });
        self.store.insert(file.to_string(), value.clone());
        self.pending.insert(file.to_string(), value);
        self.dirty = true;
    }

    pub fn set_segment(&mut self, file: &str, record: Value) {
        self.store.insert(file.to_string(), record.clone());
        self.pending.insert(file.to_string(), record);
        self.dirty = true;
    }

    /// Publish pending records. The lock protects the entire read-modify-write
    /// transaction, not merely the final rename.
    pub fn flush(&mut self) -> Result<()> {
        if !self.dirty {
            return Ok(());
        }
        fs::create_dir_all(&self.data_dir)?;
        let token = acquire_lock(&self.data_dir, &self.lock_path)?;
        let outcome = (|| -> Result<Map<String, Value>> {
            let mut merged = read_store(&self.file_path)?;
            for (file, value) in &self.pending {
                merged.insert(file.clone(), value.clone());
            }
            let tmp_path = PathBuf::from(format!(
                "{}.tmp-{}-{}",
                self.file_path.display(),
                std::process::id(),
                uuid::Uuid::new_v4()
            ));
            let write = (|| -> Result<()> {
                let mut file = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&tmp_path)?;
                file.write_all(
                    serde_json::to_string_pretty(&Value::Object(merged.clone()))?.as_bytes(),
                )?;
                file.sync_all()?;
                drop(file);
                fs::rename(&tmp_path, &self.file_path)?;
                sync_directory(&self.data_dir)?;
                Ok(())
            })();
            if let Err(error) = write {
                let _ = fs::remove_file(&tmp_path);
                return Err(error);
            }
            Ok(merged)
        })();
        release_lock(&self.data_dir, &self.lock_path, &token);
        let merged = outcome?;
        self.store = merged;
        self.pending.clear();
        self.dirty = false;
        Ok(())
    }
}
