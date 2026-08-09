//! Error type, process helpers, and the small output primitives every command
//! shares. The error carries one operator-facing sentence; `main` prefixes it
//! with `error: ` exactly as the previous implementation did, because the
//! documented failure paths in docs/ and examples/ quote those strings.
use std::env;
use std::ffi::OsStr;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug)]
pub struct Error(pub String);

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Error {}

impl From<String> for Error {
    fn from(value: String) -> Self {
        Error(value)
    }
}

impl From<&str> for Error {
    fn from(value: &str) -> Self {
        Error(value.to_string())
    }
}

impl From<std::io::Error> for Error {
    fn from(value: std::io::Error) -> Self {
        Error(value.to_string())
    }
}

impl From<serde_json::Error> for Error {
    fn from(value: serde_json::Error) -> Self {
        Error(value.to_string())
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Fail with a formatted message: `bail!("unknown source {name}")`.
#[macro_export]
macro_rules! bail {
    ($($arg:tt)*) => {
        return Err($crate::util::Error(format!($($arg)*)))
    };
}

/// Single-quote a SQL literal by doubling embedded quotes.
pub fn quote_sql(value: impl AsRef<str>) -> String {
    format!("'{}'", value.as_ref().replace('\'', "''"))
}

/// Print a value as two-space-indented JSON with a trailing newline.
pub fn write_json<T: serde::Serialize>(value: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

/// Run a child process with inherited stdio and return its exit status.
pub fn run_binary<S: AsRef<OsStr>>(name: &str, args: &[S]) -> Result<i32> {
    let status = Command::new(name)
        .args(args)
        .status()
        .map_err(|error| Error(format!("{name} failed to start: {error}")))?;
    match status.code() {
        Some(code) => Ok(code),
        None => Err(Error(format!("{name} terminated by signal"))),
    }
}

/// First directory on PATH holding an executable of this name.
pub fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;
    env::split_paths(&path).find_map(|dir| {
        if dir.as_os_str().is_empty() {
            return None;
        }
        let candidate = dir.join(name);
        candidate.exists().then_some(candidate)
    })
}

/// Absolute path without touching the filesystem, mirroring Node's `resolve`.
pub fn absolute(path: impl AsRef<Path>) -> PathBuf {
    let path = path.as_ref();
    if path.is_absolute() {
        return path.to_path_buf();
    }
    env::current_dir().unwrap_or_else(|_| PathBuf::from(".")).join(path)
}

/// `$HOME`, or `.` when the environment has none.
pub fn home_dir() -> PathBuf {
    env::var_os("HOME").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
}

/// Local host name as the cursor lease and canonical events record it.
pub fn machine_name() -> String {
    gethostname::gethostname().to_string_lossy().to_string()
}

/// Current instant as an ISO-8601 UTC string with millisecond precision.
pub fn now_iso() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string()
}

/// Milliseconds since the Unix epoch for a filesystem timestamp.
///
/// Computed exactly as libuv computes `stat.mtimeMs` — whole seconds scaled,
/// then the nanosecond remainder scaled — because existing cursor records hold
/// that value and the resume check compares it for equality. Summing first and
/// scaling once (`as_secs_f64() * 1000.0`) rounds differently for a third of
/// the real store, which would read as a source rewritten in place.
pub fn mtime_ms(meta: &std::fs::Metadata) -> f64 {
    use std::os::unix::fs::MetadataExt;
    meta.mtime() as f64 * 1000.0 + meta.mtime_nsec() as f64 / 1_000_000.0
}
