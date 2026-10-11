//! One segment, claimed: who holds it, whether that holder is still alive,
//! and how the claim is released so two concurrent runs never publish the
//! same segment twice.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{json, Value};

use crate::util::Result;

use super::super::*;

pub(super) fn process_identity(pid: u32) -> Option<String> {
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

/// `kill(pid, 0)`: `Ok(())` when the signal was deliverable, `Err(true)` for EPERM
/// (the process exists but is not ours), `Err(false)` for ESRCH and anything else.
pub(super) fn signal_zero(pid: i32) -> std::result::Result<(), bool> {
    extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
        fn __error() -> *mut i32;
    }
    // SAFETY: kill with signal zero performs an existence and permission check only.
    let (result, errno) = unsafe {
        let result = kill(pid, 0);
        (result, *__error())
    };
    if result == 0 {
        return Ok(());
    }
    // EPERM is 1 on Darwin: the process exists and belongs to another user.
    Err(errno == 1)
}

pub(super) fn owner_alive(owner: Option<&Value>, segment_id: &str) -> bool {
    let Some(owner) = owner else {
        return false;
    };
    let Some(pid) = owner.get("pid").and_then(Value::as_i64) else {
        return false;
    };
    if let Some(host) = string_field(owner, "host") {
        if host != machine_name() {
            return true;
        }
    }
    let current_identity = process_identity(pid as u32);
    if let (Some(started), Some(current)) = (string_field(owner, "started"), &current_identity) {
        return &started == current;
    }
    match signal_zero(pid as i32) {
        Ok(()) => {
            warn(&format!(
                "hook segment claim identity unavailable; retaining live pid claim: {segment_id}"
            ));
            true
        }
        Err(exists) => exists,
    }
}

/// A per-segment claim so two concurrent runs never publish the same segment twice.
pub(super) struct Claim {
    path: PathBuf,
    nonce: String,
}

pub(super) fn acquire_claim(root: &Path, segment_id: &str) -> Result<Option<Claim>> {
    fs::create_dir_all(root)?;
    let path = root.join(format!("{segment_id}.claim"));
    for retry in [false, true] {
        let nonce = uuid::Uuid::new_v4().to_string();
        let owner = json!({
            "host": machine_name(),
            "pid": std::process::id(),
            "started": process_identity(std::process::id()),
            "nonce": nonce,
        });
        let temporary = root.join(format!(".{segment_id}.{nonce}.claim"));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(format!("{}\n", serde_json::to_string(&owner)?).as_bytes())?;
        file.sync_all()?;
        drop(file);
        match fs::hard_link(&temporary, &path) {
            Ok(()) => {
                let _ = fs::remove_file(&temporary);
                sync_directory(root)?;
                return Ok(Some(Claim { path, nonce }));
            }
            Err(error) => {
                let _ = fs::remove_file(&temporary);
                if error.kind() != std::io::ErrorKind::AlreadyExists {
                    return Err(error.into());
                }
                // A claim that cannot be read is not a dead one: removing it
                // let two processes commit one segment. A claim gone since
                // the link failed is retried.
                let incumbent = match fs::read_to_string(&path) {
                    Ok(raw) => parse(&raw),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(error) => {
                        return Err(Error(format!(
                            "cannot read hook segment claim {}: {error}",
                            path.display()
                        )))
                    }
                };
                if owner_alive(incumbent.as_ref(), segment_id) {
                    warn(&format!("hook segment already claimed: {segment_id}"));
                    return Ok(None);
                }
                if let Err(error) = fs::remove_file(&path) {
                    return Err(Error(format!(
                        "cannot remove the stale hook segment claim {}: {error}",
                        path.display()
                    )));
                }
                sync_directory(root)?;
                if retry {
                    return Ok(None);
                }
            }
        }
    }
    Ok(None)
}

/// Release a claim this run holds. A claim file that is already gone was
/// released; one that cannot be read, or cannot be removed, is said with its
/// path and cause, because the segment then stays claimed until its owner is
/// seen dead.
pub(super) fn release_claim(claim: &Claim) {
    let raw = match fs::read_to_string(&claim.path) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(error) => {
            warn(&format!(
                "hook segment claim {} cannot be read to release it: {error}",
                claim.path.display()
            ));
            return;
        }
    };
    let held = parse(&raw)
        .as_ref()
        .and_then(|owner| string_field(owner, "nonce"));
    if held.as_deref() != Some(claim.nonce.as_str()) {
        return;
    }
    match fs::remove_file(&claim.path) {
        Ok(()) => {
            if let Some(parent) = claim.path.parent() {
                let _ = sync_directory(parent);
            }
        }
        Err(error) => warn(&format!(
            "hook segment claim {} cannot be released: {error}",
            claim.path.display()
        )),
    }
}
