//! The launch agent that ran a second `transcript-lake stream` beside the
//! declared service, retired when the declared service starts.
//!
//! A host ran two identical streamers: the hand-made
//! `com.wisent.transcript-lake-stream` and the declared
//! `com.wisent.compute.service.transcript-lake`. Only one holds the writer
//! lease at a time, so the other kept a process and a watcher for nothing.
//! Started by launchd as the declared unit, `stream` boots the hand-made unit
//! out and removes its launch agent, so no login loads it again. Run by hand
//! or by a test, it retires nothing.

/// The label the fleet runs the one Transcript Lake process under.
pub(super) const DECLARED_UNIT: &str = "com.wisent.compute.service.transcript-lake";

/// The unit whose work the declared service does.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const PREDECESSOR: &str = "com.wisent.transcript-lake-stream";

pub(super) fn retire() {
    let declared = std::env::var("XPC_SERVICE_NAME").is_ok_and(|label| label == DECLARED_UNIT);
    if declared {
        launchd();
    }
}

#[cfg(target_os = "macos")]
fn launchd() {
    use std::path::PathBuf;
    use std::process::Command;

    extern "C" {
        fn getuid() -> u32;
    }
    // SAFETY: getuid has no preconditions and cannot fail.
    let target = format!("gui/{}/{PREDECESSOR}", unsafe { getuid() });
    let plist = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join("Library/LaunchAgents")
        .join(format!("{PREDECESSOR}.plist"));
    let loaded = Command::new("/bin/launchctl")
        .arg("print")
        .arg(&target)
        .output()
        .is_ok_and(|output| output.status.success());
    if !loaded && !plist.exists() {
        return;
    }
    let _ = Command::new("/bin/launchctl")
        .arg("bootout")
        .arg(&target)
        .output();
    match std::fs::remove_file(&plist) {
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => eprintln!(
            "stream: {PREDECESSOR} is stopped but {} stays: {error}",
            plist.display()
        ),
        _ => eprintln!(
            "stream: retired {PREDECESSOR}: this service is the host's one Transcript Lake process"
        ),
    }
}

#[cfg(not(target_os = "macos"))]
fn launchd() {}
