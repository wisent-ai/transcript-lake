//! The append-only Lake partitions this export reads, measured the way the
//! export cursor records them, and the line reader that walks one byte range.

use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::num::NonZeroU64;
use std::path::{Path, PathBuf};

use crate::util::{mtime_ms, Error, Result};

/// Directory entries, treating a missing or non-directory path as empty.
/// Sorted by name: `readdirSync` returns strcmp order, and the export walk is
/// observable in the file Oko reads, so the order is part of the contract.
pub(crate) fn read_dir_names(dir: &Path) -> Result<Vec<String>> {
    match fs::read_dir(dir) {
        Ok(entries) => {
            // An entry the directory cannot list is the error, never skipped.
            let mut names = entries
                .map(|entry| entry.map(|entry| entry.file_name().to_string_lossy().into_owned()))
                .collect::<std::io::Result<Vec<String>>>()?;
            names.sort_unstable_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
            Ok(names)
        }
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
            ) =>
        {
            Ok(Vec::new())
        }
        Err(error) => Err(error.into()),
    }
}

/// Offset just past the last complete line, so a partition still being
/// appended to is read only up to its last durable record separator. The file
/// is read backwards in the filesystem's own preferred I/O size.
fn newline_aligned_size(path: &Path, size: u64) -> Result<u64> {
    use std::os::unix::fs::MetadataExt;
    let handle = File::open(path)?;
    let Some(block) = NonZeroU64::new(handle.metadata()?.blksize()) else {
        return Err(Error(format!(
            "{} reports no preferred I/O size to read it back by",
            path.display()
        )));
    };
    let mut buffer = Vec::new();
    let mut end = size;
    loop {
        let start = end.saturating_sub(block.get());
        // An empty range is the start of the file reached with no separator:
        // no line of it is complete yet.
        if start == end {
            return Ok(start);
        }
        let mut reader = &handle;
        reader.seek(SeekFrom::Start(start))?;
        buffer.clear();
        reader.take(end - start).read_to_end(&mut buffer)?;
        if buffer.len() as u64 != end - start {
            return Err(Error(format!(
                "{} shrank while its last complete line was being found",
                path.display()
            )));
        }
        if let Some(at) = buffer.iter().rposition(|byte| *byte == b'\n') {
            return Ok(start + buffer[..=at].len() as u64);
        }
        end = start;
    }
}

/// One append-only partition file, measured the way the export cursor records it.
pub(crate) struct Partition {
    pub(crate) runtime: String,
    pub(crate) path: PathBuf,
    pub(crate) size: u64,
    pub(crate) physical_size: u64,
    pub(crate) mtime_ms: f64,
}

// Oko imports this materialized per-session view. Lake remains the sole parser
// of vendor formats; Oko decodes the stable canonical rows written here.
pub(crate) fn event_partition_files(data_dir: &Path) -> Result<Vec<Partition>> {
    let mut files = Vec::new();
    let events_root = data_dir.join("events");
    for runtime_name in read_dir_names(&events_root)? {
        let Some(runtime) = runtime_name
            .strip_prefix("runtime=")
            .filter(|runtime| *runtime != "hooks")
        else {
            continue;
        };
        let runtime_dir = events_root.join(&runtime_name);
        for date_name in read_dir_names(&runtime_dir)? {
            if !date_name.starts_with("date=") {
                continue;
            }
            let date_dir = runtime_dir.join(&date_name);
            for part_name in read_dir_names(&date_dir)? {
                if !part_name.starts_with("part-") || !part_name.ends_with(".ndjson") {
                    continue;
                }
                let path = date_dir.join(&part_name);
                let meta = fs::metadata(&path)?;
                files.push(Partition {
                    runtime: runtime.to_string(),
                    size: newline_aligned_size(&path, meta.len())?,
                    physical_size: meta.len(),
                    mtime_ms: mtime_ms(&meta),
                    path,
                });
            }
        }
    }
    files.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(files)
}

/// Lines of one byte range of a partition, decoded the way Node's utf8 stream
/// decodes them: an invalid sequence becomes a replacement character rather
/// than failing the export.
pub(crate) struct LineReader {
    inner: std::io::Take<BufReader<File>>,
    buffer: Vec<u8>,
}

impl LineReader {
    /// The lines between `start` (the file's beginning when none) and `end`.
    pub(crate) fn open(path: &Path, start: Option<u64>, end: u64) -> Result<Self> {
        let mut file = File::open(path)?;
        let mut remaining = end;
        if let Some(start) = start {
            file.seek(SeekFrom::Start(start))?;
            remaining = end.saturating_sub(start);
        }
        Ok(Self {
            inner: BufReader::new(file).take(remaining),
            buffer: Vec::new(),
        })
    }

    pub(crate) fn next_line(&mut self) -> Result<Option<String>> {
        self.buffer.clear();
        self.inner.read_until(b'\n', &mut self.buffer)?;
        if self.buffer.is_empty() {
            return Ok(None);
        }
        let mut line = self.buffer.as_slice();
        if let Some(body) = line.strip_suffix(b"\n") {
            line = body;
        }
        if let Some(body) = line.strip_suffix(b"\r") {
            line = body;
        }
        Ok(Some(String::from_utf8_lossy(line).into_owned()))
    }
}
