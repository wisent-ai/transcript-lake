//! The append-only Lake partitions this export reads, measured the way the
//! export cursor records them, and the line reader that walks one byte range.

use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::util::{mtime_ms, Result};

const READ_CHUNK: usize = 65536;

/// Directory entries, treating a missing or non-directory path as empty.
/// Sorted by name: `readdirSync` returns strcmp order, and the export walk is
/// observable in the file Oko reads, so the order is part of the contract.
pub(crate) fn read_dir_names(dir: &Path) -> Result<Vec<String>> {
    match fs::read_dir(dir) {
        Ok(entries) => {
            let mut names: Vec<String> = entries
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect();
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
/// appended to is read only up to its last durable record separator.
fn newline_aligned_size(path: &Path, size: u64) -> Result<u64> {
    if size == 0 {
        return Ok(0);
    }
    let handle = File::open(path)?;
    let mut buffer = vec![0u8; std::cmp::min(READ_CHUNK as u64, size) as usize];
    let mut end = size;
    while end > 0 {
        let start = end.saturating_sub(buffer.len() as u64);
        let length = (end - start) as usize;
        read_exact_at(&handle, &mut buffer[..length], start)?;
        if let Some(at) = buffer[..length].iter().rposition(|byte| *byte == b'\n') {
            return Ok(start + at as u64 + 1);
        }
        end = start;
    }
    Ok(0)
}

fn read_exact_at(handle: &File, buffer: &mut [u8], offset: u64) -> Result<()> {
    use std::os::unix::fs::FileExt;
    handle.read_exact_at(buffer, offset)?;
    Ok(())
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
        if !runtime_name.starts_with("runtime=") || runtime_name == "runtime=hooks" {
            continue;
        }
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
                    runtime: runtime_name["runtime=".len()..].to_string(),
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
    pub(crate) fn open(path: &Path, start: u64, end: u64) -> Result<Self> {
        let mut file = File::open(path)?;
        if start > 0 {
            file.seek(SeekFrom::Start(start))?;
        }
        Ok(Self {
            inner: BufReader::new(file).take(end.saturating_sub(start)),
            buffer: Vec::with_capacity(4096),
        })
    }

    pub(crate) fn next_line(&mut self) -> Result<Option<String>> {
        self.buffer.clear();
        if self.inner.read_until(b'\n', &mut self.buffer)? == 0 {
            return Ok(None);
        }
        let mut line = self.buffer.as_slice();
        if line.last() == Some(&b'\n') {
            line = &line[..line.len() - 1];
        }
        if line.last() == Some(&b'\r') {
            line = &line[..line.len() - 1];
        }
        Ok(Some(String::from_utf8_lossy(line).into_owned()))
    }
}
