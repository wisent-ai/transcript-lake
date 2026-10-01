//! The units that ran Transcript Lake beside or before the declared service,
//! retired when the declared service starts.
//!
//! A host ran several streamers: the hand-made
//! `com.wisent.transcript-lake-stream`, the Stado-minted
//! `com.wisent.compute.service.transcript-lake` from before the catalog named
//! the unit, the daily Claude transcript sweep `ai.wisent.claude-transcripts-push`
//! and an hourly secret scrub whose work is retired, not moved.
//! Only one streamer holds the writer lease at a time, so every other one
//! kept a process and a watcher for nothing. Started by launchd as the
//! declared unit, `stream` boots each predecessor out and removes its launch
//! agent, so no login loads it again. Run by hand or by a test, it retires
//! nothing.

/// The one unit the fleet runs Transcript Lake under, as the Stado catalog
/// names it.
pub(super) const DECLARED_UNIT: &str = "com.wisent.transcript-lake";

/// The units whose work the declared service does or has retired: the
/// catalog's retired units of Transcript Lake, in the same order.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
const PREDECESSORS: [&str; 4] = [
    "com.wisent.compute.service.transcript-lake",
    "com.wisent.transcript-lake-stream",
    "com.wisent.transcript-lake-secret-scrub",
    "ai.wisent.claude-transcripts-push",
];

pub(super) fn retire() {
    let declared = std::env::var("XPC_SERVICE_NAME").is_ok_and(|label| label == DECLARED_UNIT);
    if declared {
        for predecessor in PREDECESSORS {
            launchd(predecessor);
        }
    }
}

#[cfg(target_os = "macos")]
fn launchd(predecessor: &str) {
    use std::path::PathBuf;
    use std::process::Command;

    extern "C" {
        fn getuid() -> u32;
    }
    // SAFETY: getuid has no preconditions and cannot fail.
    let target = format!("gui/{}/{predecessor}", unsafe { getuid() });
    let plist = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join("Library/LaunchAgents")
        .join(format!("{predecessor}.plist"));
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
            "stream: {predecessor} is stopped but {} stays: {error}",
            plist.display()
        ),
        _ => eprintln!(
            "stream: retired {predecessor}: this service is the host's one Transcript Lake process"
        ),
    }
}

#[cfg(not(target_os = "macos"))]
fn launchd(_predecessor: &str) {}
