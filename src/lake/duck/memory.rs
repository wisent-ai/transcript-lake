//! How much memory a DuckDB statement may take, measured when it starts.
//!
//! Left to itself DuckDB grants every process 80% of the machine's physical
//! memory, whatever the rest of the machine already holds, and runs one
//! worker per core, each holding its own aggregation state. A whole-Lake
//! statement over many gigabytes of NDJSON then grows into memory other
//! processes occupy until the kernel can no longer page, and the machine
//! stops. The limit is therefore the memory the kernel reports as available
//! at the moment the statement starts; DuckDB spills what does not fit to a
//! directory under the Lake root instead of the caller's working directory,
//! and it does not keep rows in insertion order, which an aggregate never
//! needs and which forbids spilling. Measured on a 42 GB Lake, the canonical
//! whole-Lake digest fails with "Out of Memory" under the default of one
//! worker per core and finishes inside the limit with four workers. The
//! figure and how it was obtained are said on stderr, so a statement refused
//! for memory names the limit it ran under.
//!
//! The available memory is one figure for every process that measures it, so
//! statements started together each took all of it: seven Oko readers of one
//! Lake held fourteen to seventeen gigabytes each on a 64 GB machine, swap
//! filled, and every program on it stood still. A statement therefore holds
//! the Lake root's statement lock (`duckdb-statement.lock`) from before its
//! memory is measured until DuckDB ends; another statement on the same root
//! waits for it, saying on stderr whom it waits for, and measures the memory
//! that statement left. Statements on two Lake roots of one machine do not
//! wait for each other.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::path::Path;

use crate::util::{quote_sql, Error, Result};

/// The Lake root's statement lock, held while one DuckDB statement runs;
/// released when dropped (the descriptor closes, and the kernel releases it
/// too if this process ends). Rust opens files close-on-exec, so DuckDB does
/// not inherit it.
pub(crate) struct Slot {
    _file: File,
}

fn locked(file: &File, flags: libc::c_int) -> std::io::Result<bool> {
    loop {
        // SAFETY: flock takes a descriptor this File owns and changes no memory.
        if unsafe { libc::flock(file.as_raw_fd(), flags) } == 0 {
            return Ok(true);
        }
        let error = std::io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::EINTR) => continue,
            Some(libc::EWOULDBLOCK) if flags & libc::LOCK_NB != 0 => return Ok(false),
            _ => return Err(error),
        }
    }
}

/// Take the statement lock of the Lake root `data_dir`, waiting for the
/// statement that holds it, and record this process as its holder.
pub(crate) fn slot(data_dir: &Path) -> Result<Slot> {
    std::fs::create_dir_all(data_dir).map_err(|error| {
        Error(format!("the Lake root {} cannot be created for its statement lock: {error}", data_dir.display()))
    })?;
    let path = data_dir.join("duckdb-statement.lock");
    let mut file = OpenOptions::new().create(true).truncate(false).read(true).write(true).open(&path).map_err(|error| {
        Error(format!("the DuckDB statement lock {} cannot be opened: {error}", path.display()))
    })?;
    let refused = |error: std::io::Error| Error(format!("the DuckDB statement lock {} cannot be taken: {error}", path.display()));
    if !locked(&file, libc::LOCK_EX | libc::LOCK_NB).map_err(refused)? {
        let mut holder = String::new();
        let _ = file.read_to_string(&mut holder);
        let holder = match holder.trim() {
            "" => "a statement that recorded no holder".to_string(),
            named => named.to_string(),
        };
        stado_wait::blocking(
            stado_wait::Kind::Lock,
            format!("the DuckDB statement of {holder}, so this one runs within the memory it leaves"),
            path.display(),
            || locked(&file, libc::LOCK_EX),
        )
        .map_err(refused)?;
    }
    let command: Vec<String> = std::env::args().take(3).collect();
    let record = format!(
        "pid {} ({}) since {}\n",
        std::process::id(),
        command.join(" "),
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    );
    file.set_len(0)
        .and_then(|()| file.seek(SeekFrom::Start(0)))
        .and_then(|_| file.write_all(record.as_bytes()))
        .map_err(|error| Error(format!("the DuckDB statement lock {} cannot name its holder: {error}", path.display())))?;
    Ok(Slot { _file: file })
}

/// The memory available on this machine now, in bytes, with the sentence that
/// says how it was measured.
#[cfg(target_os = "macos")]
fn available() -> Result<(u64, String)> {
    // SAFETY: host_statistics64 writes at most `count` integers into the
    // zeroed statistics record it is given and reports how many it wrote.
    let mut statistics: libc::vm_statistics64 = unsafe { std::mem::zeroed() };
    let mut count = libc::HOST_VM_INFO64_COUNT;
    #[allow(deprecated)]
    let host = unsafe { libc::mach_host_self() };
    let answered = unsafe {
        libc::host_statistics64(
            host,
            libc::HOST_VM_INFO64,
            (&mut statistics as *mut libc::vm_statistics64).cast(),
            &mut count,
        )
    };
    if answered != libc::KERN_SUCCESS {
        return Err(Error(format!(
            "cannot measure the memory available for a DuckDB statement: host_statistics64 answered kern_return {answered}"
        )));
    }
    // SAFETY: sysconf reads a configuration value and touches no memory.
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    let page = u64::try_from(page).map_err(|_| {
        Error(format!(
            "cannot measure the memory available for a DuckDB statement: the page size is {page}"
        ))
    })?;
    let pages = u64::from(statistics.free_count)
        + u64::from(statistics.inactive_count)
        + u64::from(statistics.speculative_count)
        + u64::from(statistics.purgeable_count);
    Ok((
        pages * page,
        format!(
            "free {} + inactive {} + speculative {} + purgeable {} pages of {page} bytes (host_statistics64)",
            statistics.free_count,
            statistics.inactive_count,
            statistics.speculative_count,
            statistics.purgeable_count
        ),
    ))
}

#[cfg(target_os = "linux")]
fn available() -> Result<(u64, String)> {
    let meminfo = std::fs::read_to_string("/proc/meminfo").map_err(|error| {
        Error(format!(
            "cannot measure the memory available for a DuckDB statement: /proc/meminfo cannot be read: {error}"
        ))
    })?;
    let line = meminfo
        .lines()
        .find(|line| line.starts_with("MemAvailable:"))
        .ok_or_else(|| {
            Error("cannot measure the memory available for a DuckDB statement: /proc/meminfo has no MemAvailable line"
                .into())
        })?;
    let kib: u64 = line
        .split_whitespace()
        .nth(1)
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| {
            Error(format!(
                "cannot measure the memory available for a DuckDB statement: unreadable /proc/meminfo line {line:?}"
            ))
        })?;
    Ok((kib * 1024, format!("{line} (/proc/meminfo)")))
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn available() -> Result<(u64, String)> {
    Err(Error(format!(
        "cannot measure the memory available for a DuckDB statement on {}: Transcript Lake reads it only on macOS and Linux",
        std::env::consts::OS
    )))
}

/// Workers each hold their own hash tables; this many fit the measured limit
/// on the whole-Lake digest, where one per core did not.
const THREADS: usize = 4;

/// The settings every DuckDB statement runs under: its memory limit, the
/// memory measured as available now, its worker count, no insertion order,
/// and its spill directory under the Lake root `data_dir`; with the Lake
/// root's statement lock, taken before the memory is measured, which the
/// caller holds until DuckDB ends.
pub(super) fn settings(data_dir: &Path) -> Result<(String, Slot)> {
    let slot = slot(data_dir)?;
    let (bytes, measured) = available()?;
    if bytes == 0 {
        return Err(Error(format!(
            "this machine reports no memory available for a DuckDB statement ({measured}); \
             the statement runs once other processes release memory"
        )));
    }
    let spill = data_dir.join("duckdb-spill");
    eprintln!(
        "duckdb memory limit: {bytes} bytes, the memory available now ({measured}); {THREADS} workers; spill directory {}",
        spill.display()
    );
    let script = format!(
        "SET memory_limit = '{bytes}B';\nSET threads = {THREADS};\nSET preserve_insertion_order = false;\nSET temp_directory = {};\n",
        quote_sql(spill.to_string_lossy())
    );
    Ok((script, slot))
}
