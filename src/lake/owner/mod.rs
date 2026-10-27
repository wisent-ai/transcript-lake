//! The process behind an owner record, for the leases and claims that keep
//! two writers apart: a process is its pid together with the start time the
//! system reports for it, so a later process given the same id is not taken
//! for the owner.
use std::process::Command;

use crate::util::{Error, Result};

/// The start time `/bin/ps` reports for `pid`. `None` is no process with that
/// id; a `ps` that cannot run, or that answers neither with a start time nor
/// with the absence of the process, is the error, since an owner that cannot
/// be checked must not be taken for a dead one. The wait on `ps` is said on
/// stderr.
pub fn start_time(pid: u32) -> Result<Option<String>> {
    let output =
        stado_wait::output(Command::new("/bin/ps").args(["-o", "lstart=", "-p", &pid.to_string()]))
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

/// The start time of this process, which every owner record it writes
/// carries. Without one its record could not be told from a later process
/// given the same id, so `what` (the lease or claim being taken) is refused.
pub fn own_start_time(what: &str) -> Result<String> {
    let pid = std::process::id();
    start_time(pid)?.ok_or_else(|| {
        Error(format!(
            "/bin/ps reports no start time for this process {pid}, so {what} could not be told from a later process with that id"
        ))
    })
}

/// Whether the owner recorded as `pid` started at `started` on `host` still
/// runs. An owner on another machine cannot be seen from here and is taken
/// as alive, since breaking its hold would let two writers publish the same
/// data.
pub fn alive(host: &str, pid: u32, started: &str) -> Result<bool> {
    if host != crate::util::machine_name() {
        return Ok(true);
    }
    Ok(start_time(pid)?.as_deref() == Some(started))
}

/// This process as an owner record names it, {host, pid, started}, for a
/// record `what` that a later reader checks with [`alive`].
pub fn own_record(what: &str) -> Result<serde_json::Value> {
    Ok(serde_json::json!({
        "host": crate::util::machine_name(),
        "pid": std::process::id(),
        "started": own_start_time(what)?,
    }))
}
