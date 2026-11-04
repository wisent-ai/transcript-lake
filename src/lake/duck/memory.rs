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

use std::path::Path;

use crate::util::{quote_sql, Error, Result};

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
/// and its spill directory under the Lake root `data_dir`.
pub(super) fn settings(data_dir: &Path) -> Result<String> {
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
    Ok(format!(
        "SET memory_limit = '{bytes}B';\nSET threads = {THREADS};\nSET preserve_insertion_order = false;\nSET temp_directory = {};\n",
        quote_sql(spill.to_string_lossy())
    ))
}
