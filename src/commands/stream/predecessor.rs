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
const PREDECESSORS: &[&str] = &[
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

/// Boot one predecessor out and remove its launch agent. Each launchctl call
/// is announced through the shared wait mechanism while it stands, and a
/// call that cannot run, a bootout launchd refuses and a launch agent that
/// cannot be examined are each said with their cause; the predecessor is
/// then left as it is rather than reported retired.
#[cfg(target_os = "macos")]
fn launchd(predecessor: &str) {
    use std::process::Command;

    extern "C" {
        fn getuid() -> u32;
    }
    // SAFETY: getuid has no preconditions and cannot fail.
    let target = format!("gui/{}/{predecessor}", unsafe { getuid() });
    let plist = crate::util::home_dir()
        .join("Library/LaunchAgents")
        .join(format!("{predecessor}.plist"));
    let loaded = match stado_wait::output(Command::new("/bin/launchctl").arg("print").arg(&target))
    {
        Ok(output) => output.status.success(),
        Err(error) => {
            eprintln!(
                "stream: {predecessor} stays: launchctl print {target} could not run: {error}"
            );
            return;
        }
    };
    let present = match std::fs::symlink_metadata(&plist) {
        Ok(_) => true,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => false,
        Err(error) => {
            eprintln!(
                "stream: {predecessor} stays: {} cannot be examined: {error}",
                plist.display()
            );
            return;
        }
    };
    if !loaded && !present {
        return;
    }
    if loaded {
        match stado_wait::output(Command::new("/bin/launchctl").arg("bootout").arg(&target)) {
            Ok(output) if output.status.success() => {}
            Ok(output) => {
                eprintln!(
                    "stream: {predecessor} stays: launchctl bootout {target} ended {}: {}",
                    output.status,
                    String::from_utf8_lossy(&output.stderr).trim()
                );
                return;
            }
            Err(error) => {
                eprintln!(
                    "stream: {predecessor} stays: launchctl bootout {target} could not run: {error}"
                );
                return;
            }
        }
    }
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
