//! Local snapshot-and-delta stream for native transcript consumers.
//!
//! The stream command owns this socket. Canonical events are masked before they
//! reach this module; clients never receive vendor transcript bytes.
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{sync_channel, SyncSender};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::util::{Error, Result};

pub const SOCKET_FILE: &str = "live.sock";
const STATE_FILE: &str = "live-state.json";
const PRESENCE_WAKE: usize = 1;
const SUBSCRIBER_BUFFER: usize = 64;

static HUB: OnceLock<Arc<Hub>> = OnceLock::new();

#[derive(Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct DurableState {
    revision: u64,
    active_source_hashes: BTreeSet<String>,
}

struct HubState {
    durable: DurableState,
    subscribers: Vec<SyncSender<String>>,
}

struct Hub {
    data_dir: PathBuf,
    state: Mutex<HubState>,
    presence_queue: i32,
}

/// Start the process-local Unix socket before source catch-up begins. A client
/// can therefore obtain presence immediately even while historical cursor gaps
/// are still being closed.
pub fn start(data_dir: &Path) -> Result<()> {
    if HUB.get().is_some() {
        return Ok(());
    }
    fs::create_dir_all(data_dir)?;
    let socket_path = data_dir.join(SOCKET_FILE);
    match fs::remove_file(&socket_path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let listener = UnixListener::bind(&socket_path)
        .map_err(|error| Error(format!("live stream could not bind {}: {error}", socket_path.display())))?;
    fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o600))?;

    let presence_queue = unsafe { libc::kqueue() };
    if presence_queue < 0 {
        return Err(Error(format!(
            "presence stream could not create kqueue: {}",
            std::io::Error::last_os_error()
        )));
    }
    register_presence_wake(presence_queue)?;
    let durable = read_state(data_dir);
    let hub = Arc::new(Hub {
        data_dir: data_dir.to_path_buf(),
        presence_queue,
        state: Mutex::new(HubState {
            durable,
            subscribers: Vec::new(),
        }),
    });
    HUB.set(Arc::clone(&hub))
        .map_err(|_| Error("live stream was initialized twice".into()))?;

    let accept_hub = Arc::clone(&hub);
    thread::Builder::new()
        .name("transcript-lake-live".into())
        .spawn(move || accept_loop(listener, accept_hub))
        .map_err(|error| Error(format!("live stream could not start: {error}")))?;

    let presence_hub = Arc::clone(&hub);
    thread::Builder::new()
        .name("transcript-lake-presence".into())
        .spawn(move || presence_loop(presence_hub))
        .map_err(|error| Error(format!("presence stream could not start: {error}")))?;
    Ok(())
}

/// Publish masked Oko-compatible rows after the canonical append succeeds.
/// Deterministic UUIDs make a retry harmless to clients.
pub fn publish_events(data_dir: &Path, events: &[Value]) {
    let Some(hub) = HUB.get() else { return };
    let rows = events
        .iter()
        .filter_map(|event| crate::oko_export::live_row(data_dir, event))
        .collect::<Vec<Value>>();
    if rows.is_empty() {
        return;
    }
    hub.publish(rows, None);
    wake_presence(hub.presence_queue);
}

impl Hub {
    fn publish(&self, events: Vec<Value>, active: Option<BTreeSet<String>>) {
        let mut state = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(active) = active {
            if active == state.durable.active_source_hashes {
                return;
            }
            state.durable.active_source_hashes = active;
        }
        state.durable.revision = state.durable.revision.wrapping_add(1);
        if let Err(error) = write_state(&self.data_dir, &state.durable) {
            crate::stream::warn(&format!("live state: {error}"));
            return;
        }
        let line = envelope("delta", &state.durable, events);
        state.subscribers.retain(|subscriber| subscriber.try_send(line.clone()).is_ok());
    }

    fn subscribe(&self, stream: UnixStream) {
        let (sender, receiver) = sync_channel::<String>(SUBSCRIBER_BUFFER);
        {
            let mut state = self.state.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            let snapshot = envelope("snapshot", &state.durable, Vec::new());
            if sender.try_send(snapshot).is_err() {
                return;
            }
            state.subscribers.push(sender);
        }
        let _ = thread::Builder::new()
            .name("transcript-lake-subscriber".into())
            .spawn(move || {
                let mut stream = stream;
                for line in receiver {
                    if stream.write_all(line.as_bytes()).is_err() {
                        break;
                    }
                }
            });
    }
}

fn envelope(kind: &str, state: &DurableState, events: Vec<Value>) -> String {
    serde_json::to_string(&json!({
        "type": kind,
        "revision": state.revision,
        "activeSourceHashes": state.active_source_hashes,
        "events": events,
    }))
    .unwrap_or_else(|_| "{\"type\":\"error\"}".into())
        + "\n"
}

fn accept_loop(listener: UnixListener, hub: Arc<Hub>) {
    for connection in listener.incoming() {
        match connection {
            Ok(stream) => hub.subscribe(stream),
            Err(error) => crate::stream::warn(&format!("live accept: {error}")),
        }
    }
}

fn presence_loop(hub: Arc<Hub>) {
    loop {
        let processes = active_sources_by_process();
        let active = processes
            .values()
            .flat_map(|hashes| hashes.iter().cloned())
            .collect();
        hub.publish(Vec::new(), Some(active));
        for pid in processes.keys() {
            watch_process_exit(hub.presence_queue, *pid);
        }

        let mut event = unsafe { std::mem::zeroed::<libc::kevent>() };
        let count = unsafe {
            libc::kevent(
                hub.presence_queue,
                std::ptr::null(),
                0,
                &mut event,
                1,
                std::ptr::null(),
            )
        };
        if count < 0 {
            crate::stream::warn(&format!(
                "presence wait: {}",
                std::io::Error::last_os_error()
            ));
            thread::yield_now();
        }
    }
}

fn active_sources_by_process() -> BTreeMap<i32, BTreeSet<String>> {
    let output = Command::new("/usr/sbin/lsof")
        .args(["-F", "pn", "-c", "omp", "-c", "jeden"])
        .output();
    let Ok(output) = output else { return BTreeMap::new() };
    let text = String::from_utf8_lossy(&output.stdout);
    let mut processes = BTreeMap::<i32, BTreeSet<String>>::new();
    let mut pid = None;
    for line in text.lines() {
        if let Some(raw) = line.strip_prefix('p') {
            pid = raw.parse().ok();
            continue;
        }
        let Some(path) = line.strip_prefix('n') else { continue };
        let is_session = path.contains("/.omp/agent/sessions/")
            || path.contains("/.jeden/sessions/");
        if !is_session || !path.ends_with(".jsonl") {
            continue;
        }
        let Some(pid) = pid else { continue };
        let Some(stem) = Path::new(path).file_stem() else { continue };
        processes.entry(pid).or_default().insert(format!(
            "{:x}",
            Sha256::digest(stem.to_string_lossy().as_bytes())
        ));
    }
    processes
}

fn register_presence_wake(queue: i32) -> Result<()> {
    let change = kernel_event(
        PRESENCE_WAKE,
        libc::EVFILT_USER,
        (libc::EV_ADD | libc::EV_CLEAR) as u16,
        0,
    );
    let result = unsafe {
        libc::kevent(
            queue,
            &change,
            1,
            std::ptr::null_mut(),
            0,
            std::ptr::null(),
        )
    };
    if result < 0 {
        return Err(Error(format!(
            "presence stream could not register wake event: {}",
            std::io::Error::last_os_error()
        )));
    }
    Ok(())
}

fn wake_presence(queue: i32) {
    let change = kernel_event(PRESENCE_WAKE, libc::EVFILT_USER, 0, libc::NOTE_TRIGGER);
    unsafe {
        libc::kevent(
            queue,
            &change,
            1,
            std::ptr::null_mut(),
            0,
            std::ptr::null(),
        );
    }
}

fn watch_process_exit(queue: i32, pid: i32) {
    let change = kernel_event(
        pid as usize,
        libc::EVFILT_PROC,
        (libc::EV_ADD | libc::EV_ENABLE | libc::EV_ONESHOT) as u16,
        libc::NOTE_EXIT,
    );
    unsafe {
        libc::kevent(
            queue,
            &change,
            1,
            std::ptr::null_mut(),
            0,
            std::ptr::null(),
        );
    }
}

fn kernel_event(ident: usize, filter: i16, flags: u16, fflags: u32) -> libc::kevent {
    let mut event = unsafe { std::mem::zeroed::<libc::kevent>() };
    event.ident = ident;
    event.filter = filter;
    event.flags = flags;
    event.fflags = fflags;
    event
}

fn read_state(data_dir: &Path) -> DurableState {
    let path = data_dir.join(STATE_FILE);
    fs::read(&path)
        .ok()
        .and_then(|data| serde_json::from_slice(&data).ok())
        .unwrap_or_default()
}

fn write_state(data_dir: &Path, state: &DurableState) -> Result<()> {
    let path = data_dir.join(STATE_FILE);
    let temporary = data_dir.join(format!("{STATE_FILE}.tmp-{}", std::process::id()));
    fs::write(&temporary, serde_json::to_vec(state)?)?;
    fs::rename(temporary, path)?;
    Ok(())
}
