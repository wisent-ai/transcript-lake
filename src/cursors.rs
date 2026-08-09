//! Cursor store for mutable byte streams and immutable closed segments, plus
//! the single-writer state lease. Legacy records retain their numeric coercion
//! contract; tagged segment commits are stored as structured records and never
//! projected onto byte-offset fields. Writes are durable: unique temp, file
//! sync, rename, then parent sync.
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{json, Map, Value};

use crate::util::{machine_name, Error, Result};

const SEGMENT_KIND: &str = "closed-segment";

/// A byte-offset resume point for a mutable append-only source file.
#[derive(Debug, Clone, Copy)]
pub struct ByteCursor {
    pub mtime_ms: f64,
    pub size: u64,
    pub offset: u64,
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

fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)?.sync_all()?;
    Ok(())
}

fn process_identity(pid: u32) -> Option<String> {
    let output = Command::new("/bin/ps")
        .args(["-o", "lstart=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!text.is_empty()).then_some(text)
}

fn owner_is_alive(owner: Option<&Value>) -> bool {
    let Some(owner) = owner else {
        return false;
    };
    let Some(pid) = owner.get("pid").and_then(Value::as_u64) else {
        return false;
    };
    if pid == 0 {
        return false;
    }
    let host = owner
        .get("host")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if host != machine_name() {
        return true;
    }
    let started = owner.get("started").and_then(Value::as_str);
    let current = process_identity(pid as u32);
    match (started, current.as_deref()) {
        (Some(started), Some(current)) => started == current,
        _ => {
            // Identity unavailable: a live pid claim is retained rather than
            // stolen, because stealing a lease double-writes partitions.
            let alive = unsafe { libc_kill(pid as i32) };
            if alive {
                eprintln!("cursors: lock process identity unavailable; retaining live pid claim");
            }
            alive
        }
    }
}

/// `kill(pid, 0)`: true when the process exists (or exists but is not ours).
unsafe fn libc_kill(pid: i32) -> bool {
    extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    extern "C" {
        fn __error() -> *mut i32;
    }
    let result = kill(pid, 0);
    if result == 0 {
        return true;
    }
    // ESRCH is 3 on Darwin: no such process. Anything else (EPERM) means the
    // process exists and belongs to another user.
    *__error() != 3
}

/// Exclusive state lease over one Lake root, released on drop.
pub struct WriterLease {
    data_dir: PathBuf,
    lock_path: PathBuf,
    token: String,
    released: bool,
}

impl WriterLease {
    pub fn close(&mut self) {
        if self.released {
            return;
        }
        release_lock(&self.data_dir, &self.lock_path, &self.token);
        self.released = true;
    }
}

impl Drop for WriterLease {
    fn drop(&mut self) {
        self.close();
    }
}

/// Build and sync each claim privately, then publish it with one rename. This
/// makes an absent or malformed published owner structurally abandoned rather
/// than a live process caught between mkdir and writing its identity.
fn acquire_lock(data_dir: &Path, lock_path: &Path) -> Result<String> {
    let token = uuid::Uuid::new_v4().to_string();
    let pid = std::process::id();
    let owner = json!({
        "host": machine_name(),
        "pid": pid,
        "started": process_identity(pid),
        "token": token,
    });
    loop {
        let prepared = PathBuf::from(format!(
            "{}.claim-{pid}-{}",
            lock_path.display(),
            uuid::Uuid::new_v4()
        ));
        fs::create_dir(&prepared)?;
        let published = (|| -> Result<()> {
            let owner_path = prepared.join("owner.json");
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&owner_path)?;
            file.write_all(serde_json::to_string(&owner)?.as_bytes())?;
            file.sync_all()?;
            drop(file);
            sync_directory(&prepared)?;
            fs::rename(&prepared, lock_path)?;
            sync_directory(data_dir)?;
            Ok(())
        })();
        match published {
            Ok(()) => return Ok(token),
            Err(error) => {
                let _ = fs::remove_dir_all(&prepared);
                if !lock_path.exists() {
                    return Err(error);
                }
            }
        }

        let incumbent = fs::read_to_string(lock_path.join("owner.json"))
            .ok()
            .and_then(|raw| serde_json::from_str::<Value>(&raw).ok());
        if owner_is_alive(incumbent.as_ref()) {
            let incumbent = incumbent.unwrap_or(Value::Null);
            let host = incumbent
                .get("host")
                .and_then(Value::as_str)
                .unwrap_or("unknown-host");
            let pid = incumbent
                .get("pid")
                .and_then(Value::as_u64)
                .map(|pid| pid.to_string())
                .unwrap_or_else(|| "unknown".to_string());
            return Err(Error(format!(
                "state writer lock is held by {host} pid {pid}"
            )));
        }
        match fs::remove_dir_all(lock_path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
}

fn release_lock(data_dir: &Path, lock_path: &Path, token: &str) {
    let owner = fs::read_to_string(lock_path.join("owner.json"))
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok());
    let matches = owner
        .as_ref()
        .and_then(|owner| owner.get("token"))
        .and_then(Value::as_str)
        .is_some_and(|held| held == token);
    if !matches {
        return;
    }
    let _ = fs::remove_dir_all(lock_path);
    let _ = sync_directory(data_dir);
}

/// Take the exclusive state lease for this Lake root.
pub fn open_writer_lease(data_dir: &Path) -> Result<WriterLease> {
    fs::create_dir_all(data_dir)?;
    let lock_path = data_dir.join("stream.lock");
    let token = acquire_lock(data_dir, &lock_path)?;
    Ok(WriterLease {
        data_dir: data_dir.to_path_buf(),
        lock_path,
        token,
        released: false,
    })
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
