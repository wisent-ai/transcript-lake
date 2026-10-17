//! Durable single-writer leases and cursor-store publication locks.
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;
use serde_json::json;

use crate::util::{machine_name, Error, Result};
pub(super) fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)?.sync_all()?;
    Ok(())
}

/// The start time `/bin/ps` reports for `pid`, which tells a live owner from a
/// later process that reused its id. `None` is no process with that id; a `ps`
/// that cannot run, or that answers neither with a start time nor with the
/// absence of the process, is the error, since an owner that cannot be
/// checked must not be taken for a dead one.
fn start_time(pid: u32) -> Result<Option<String>> {
    let output = stado_wait::output(
        Command::new("/bin/ps").args(["-o", "lstart=", "-p", &pid.to_string()]),
    )
    .map_err(|error| {
        Error(format!(
            "cannot run /bin/ps to see whether process {pid} is alive: {error}"
        ))
    })?;
    let reported = String::from_utf8_lossy(&output.stdout).trim().to_string();
    match (output.status.success(), reported.is_empty()) {
        (true, false) => Ok(Some(reported)),
        (false, true) => Ok(None),
        _ => Err(Error(format!(
            "/bin/ps answered {} for process {pid} with {reported:?}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ))),
    }
}

/// The owner record a lease is published with: the machine and process that
/// hold it, the start time that tells that process from a later one with the
/// same id, and the token its release is matched by.
#[derive(Deserialize)]
struct Owner {
    host: String,
    pid: u32,
    started: String,
    token: String,
}

impl Owner {
    /// Whether the holder still runs. A holder on another machine cannot be
    /// seen from here and is taken as alive, since breaking its lease would
    /// let two writers publish the same partitions.
    fn alive(&self) -> Result<bool> {
        if self.host != machine_name() {
            return Ok(true);
        }
        Ok(start_time(self.pid)?.as_deref() == Some(self.started.as_str()))
    }
}

/// The owner record at `path`. An absent record and one that is not a whole
/// owner record are both abandoned: a lease is published by one rename of a
/// directory whose record was written and flushed first, so neither is a
/// writer caught half way. Which one it was is said. A record that exists and
/// cannot be read proves nothing about its writer, so that is the error.
fn read_owner(path: &Path) -> Result<Option<Owner>> {
    let raw = match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            eprintln!(
                "cursors: lease owner {} is absent, so the lease is abandoned",
                path.display()
            );
            return Ok(None);
        }
        Err(error) => {
            return Err(Error(format!(
                "state writer lock owner {} cannot be read: {error}",
                path.display()
            )))
        }
    };
    match serde_json::from_str::<Owner>(&raw) {
        Ok(owner) => Ok(Some(owner)),
        Err(error) => {
            eprintln!(
                "cursors: lease owner {} is not a whole owner record ({error}), so the lease is abandoned",
                path.display()
            );
            Ok(None)
        }
    }
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
pub(super) fn acquire_lock(data_dir: &Path, lock_path: &Path) -> Result<String> {
    let token = uuid::Uuid::new_v4().to_string();
    let pid = std::process::id();
    // Without its own start time this process's lease could not be told from
    // a later process given the same id, so it is not taken.
    let started = start_time(pid)?.ok_or_else(|| {
        Error(format!(
            "/bin/ps reports no start time for this process {pid}, so its lease could not be told from a later process with that id"
        ))
    })?;
    let owner = json!({
        "host": machine_name(),
        "pid": pid,
        "started": started,
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
                match fs::remove_dir_all(&prepared) {
                    Ok(()) => {}
                    Err(gone) if gone.kind() == std::io::ErrorKind::NotFound => {}
                    Err(left) => eprintln!(
                        "cursors: unpublished lease claim {} was left behind: {left}",
                        prepared.display()
                    ),
                }
                if !lock_path.exists() {
                    return Err(error);
                }
            }
        }

        if let Some(incumbent) = read_owner(&lock_path.join("owner.json"))? {
            if incumbent.alive()? {
                return Err(Error(format!(
                    "state writer lock is held by {} pid {}",
                    incumbent.host, incumbent.pid
                )));
            }
        }
        match fs::remove_dir_all(lock_path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
}

/// Release the lease this run holds. A lock already gone was released; an
/// owner record that cannot be read, or a lock that cannot be removed, is
/// said with its path and cause, because the next writer then waits on a
/// lease nobody holds until its owner is seen dead.
pub(super) fn release_lock(data_dir: &Path, lock_path: &Path, token: &str) {
    let owner_path = lock_path.join("owner.json");
    let raw = match fs::read_to_string(&owner_path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(error) => {
            eprintln!(
                "cursors: lease owner {} cannot be read to release it: {error}",
                owner_path.display()
            );
            return;
        }
    };
    let held = match serde_json::from_str::<Owner>(&raw) {
        Ok(owner) => owner.token,
        Err(error) => {
            eprintln!(
                "cursors: lease owner {} is not a whole owner record ({error}), so it is not this run's to release",
                owner_path.display()
            );
            return;
        }
    };
    if held != token {
        return;
    }
    if let Err(error) = fs::remove_dir_all(lock_path) {
        eprintln!(
            "cursors: lease {} cannot be released: {error}",
            lock_path.display()
        );
        return;
    }
    if let Err(error) = sync_directory(data_dir) {
        eprintln!(
            "cursors: the release of lease {} may not survive a crash: {error}",
            lock_path.display()
        );
    }
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
