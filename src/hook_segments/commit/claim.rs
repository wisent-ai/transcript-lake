//! One segment, claimed: who holds it, whether that holder is still alive,
//! and how the claim is released so two concurrent runs never publish the
//! same segment twice.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::json;

use crate::lake::owner;
use crate::util::Result;

use super::super::*;

/// The owner record a claim is published with: the machine and process that
/// hold it, the start time that tells that process from a later one with the
/// same id, and the nonce its release is matched by.
#[derive(Deserialize)]
struct Holder {
    host: String,
    pid: u32,
    started: String,
    nonce: String,
}

/// The holder recorded in `raw`, read from the claim at `path`. A claim is
/// published whole by one link of a draft written and flushed first, so a
/// record that is not a whole holder is abandoned, and said with why.
fn holder(raw: &str, path: &Path) -> Option<Holder> {
    match serde_json::from_str::<Holder>(raw) {
        Ok(holder) => Some(holder),
        Err(error) => {
            warn(&format!(
                "hook segment claim {} is not a whole holder record ({error}), so it is abandoned",
                path.display()
            ));
            None
        }
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
    let started = owner::own_start_time("its claim on a hook segment")?;
    for retry in [false, true] {
        let nonce = uuid::Uuid::new_v4().to_string();
        let record = json!({
            "host": machine_name(),
            "pid": std::process::id(),
            "started": started,
            "nonce": nonce,
        });
        let temporary = root.join(format!(".{segment_id}.{nonce}.claim"));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(format!("{}\n", serde_json::to_string(&record)?).as_bytes())?;
        file.sync_all()?;
        drop(file);
        match fs::hard_link(&temporary, &path) {
            Ok(()) => {
                discard_draft(&temporary);
                sync_directory(root)?;
                return Ok(Some(Claim { path, nonce }));
            }
            Err(error) => {
                discard_draft(&temporary);
                if error.kind() != std::io::ErrorKind::AlreadyExists {
                    return Err(error.into());
                }
                // A claim that cannot be read is not a dead one: removing it
                // let two processes commit one segment. A claim gone since
                // the link failed is retried.
                let incumbent = match fs::read_to_string(&path) {
                    Ok(raw) => holder(&raw, &path),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(error) => {
                        return Err(Error(format!(
                            "cannot read hook segment claim {}: {error}",
                            path.display()
                        )))
                    }
                };
                if let Some(incumbent) = incumbent {
                    if owner::alive(&incumbent.host, incumbent.pid, &incumbent.started)? {
                        warn(&format!(
                            "hook segment {segment_id} is claimed by {} pid {}",
                            incumbent.host, incumbent.pid
                        ));
                        return Ok(None);
                    }
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

/// The claim's draft is removed once linked or refused; a draft that stays is
/// said with its path and cause, since nothing else ever removes it.
fn discard_draft(temporary: &Path) {
    if let Err(error) = fs::remove_file(temporary) {
        warn(&format!(
            "hook segment claim draft {} was left behind: {error}",
            temporary.display()
        ));
    }
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
    let Some(held) = holder(&raw, &claim.path) else {
        return;
    };
    if held.nonce != claim.nonce {
        return;
    }
    match fs::remove_file(&claim.path) {
        Ok(()) => {
            if let Some(parent) = claim.path.parent() {
                if let Err(error) = sync_directory(parent) {
                    warn(&format!(
                        "the release of hook segment claim {} may not survive a crash: {error}",
                        claim.path.display()
                    ));
                }
            }
        }
        Err(error) => warn(&format!(
            "hook segment claim {} cannot be released: {error}",
            claim.path.display()
        )),
    }
}
