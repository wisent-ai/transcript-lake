//! A byte offset is usable only after its consumed prefix has been verified.
//! A session kept whole has no offset: it is read again whenever its file
//! changes, and what was archived before is recognised by occurrence.
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use super::super::source::Retained;
use super::super::{file_stem, hex_digest, warn, Tally, Writer, PART_DIGEST_LEN};
use crate::cursors::{ByteCursor, CursorRecord, Cursors, SourceCheckpoint};
use crate::types::{Adapter, ParserCtx, RawEvent, Reading, SessionEntry, WholeLoader};
use crate::util::{mtime_ms, Error, Result};

/// The partition file a source's events go to, and the digest of its file
/// stem, both derived from the source's path.
fn partition_names(entry: &SessionEntry, key: &str) -> Result<(String, String)> {
    let digest = hex_digest(key.as_bytes());
    let name = entry.file.file_name().ok_or_else(|| {
        Error(format!(
            "transcript path {} names no file, so no partition can be derived from it",
            entry.file.display()
        ))
    })?;
    Ok((
        format!("part-{}.ndjson", &digest[..PART_DIGEST_LEN]),
        hex_digest(file_stem(&name.to_string_lossy()).as_bytes()),
    ))
}

/// The file SQLite writes a database's committed pages to before they reach
/// the database file itself (its write-ahead log).
fn write_ahead_log(file: &Path) -> PathBuf {
    let mut name = file.as_os_str().to_os_string();
    name.push("-wal");
    PathBuf::from(name)
}

/// Whether a database's write-ahead log changed after `checkpoint_ms`: a
/// write there leaves the database file's own size and time untouched, so
/// the file alone would call a changed database current. A source with no
/// log has nothing beside it to change.
pub(in crate::stream) fn log_changed_since(file: &Path, checkpoint_ms: f64) -> bool {
    fs::metadata(write_ahead_log(file)).is_ok_and(|meta| mtime_ms(&meta) > checkpoint_ms)
}

/// What one file's generation is: its size and its modification time to the
/// nanosecond, as the filesystem reports them.
fn generation(meta: &fs::Metadata) -> String {
    use std::os::unix::fs::MetadataExt;
    format!("{}:{}:{}", meta.len(), meta.mtime(), meta.mtime_nsec())
}

/// The digest of a whole source's generation: the source file's and its
/// write-ahead log's when it has one. A whole source is a document or a
/// database rewritten in place, some of them hundreds of megabytes, so what
/// is compared is whether the filesystem says either changed, not their bytes.
fn whole_digest(file: &Path) -> Result<Sha256> {
    let mut hash = Sha256::new();
    hash.update(generation(&fs::metadata(file)?).as_bytes());
    let log = write_ahead_log(file);
    match fs::metadata(&log) {
        Ok(meta) => hash.update(generation(&meta).as_bytes()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(Error(format!(
                "cannot read the write-ahead log {} of {}: {error}",
                log.display(),
                file.display()
            )))
        }
    }
    Ok(hash)
}

/// Read a session kept whole when its bytes differ from the last checkpoint,
/// writing every event it holds that the source's partitions do not already
/// hold, then checkpoint the digest of what was read.
#[allow(clippy::too_many_arguments)]
fn stream_whole(
    writer: &mut Writer,
    cursors: &mut Cursors,
    runtime: &str,
    entry: &SessionEntry,
    load: WholeLoader,
    replay: bool,
    tally: &mut Tally,
    key: &str,
) -> Result<()> {
    let (part_name, stem_hash) = partition_names(entry, key)?;
    let hash = whole_digest(&entry.file)?;
    let meta = fs::metadata(&entry.file)?;
    let digest = hash.clone().finalize();
    let previous = match if replay { None } else { cursors.get(key)? } {
        Some(CursorRecord::Bytes(cursor)) => cursor.source,
        Some(CursorRecord::Segment(_)) | None => None,
    };
    let unchanged = previous.is_some_and(|source| source.sha256.as_slice() == digest.as_slice());
    let mut events = if unchanged { Vec::new() } else { load()? };
    let mut retained = Retained::load(&writer.data_dir, runtime, &part_name)?;
    checkpoint(
        writer,
        cursors,
        &mut events,
        runtime,
        &part_name,
        &stem_hash,
        tally,
        key,
        &meta,
        meta.len(),
        &hash,
        Some(&mut retained),
    )
}

fn verified_prefix(file: &mut File, offset: u64, expected: [u8; 32]) -> Result<Option<Sha256>> {
    let mut hash = Sha256::new();
    if std::io::copy(&mut Read::by_ref(file).take(offset), &mut hash)? < offset {
        return Ok(None);
    }
    let observed: [u8; 32] = hash.clone().finalize().into();
    Ok((observed == expected).then_some(hash))
}

pub(in crate::stream) fn stream_file(
    writer: &mut Writer,
    cursors: &mut Cursors,
    adapter: &dyn Adapter,
    entry: &SessionEntry,
    replay: bool,
    tally: &mut Tally,
) -> Result<()> {
    let key = entry.file.to_string_lossy().to_string();
    let runtime = adapter.runtime();
    let mut parser = match adapter.read(ParserCtx {
        file: entry.file.clone(),
        session_id: entry.session_id.clone(),
        project: entry.project.clone(),
    }) {
        Reading::Lines(parser) => parser,
        Reading::Whole(load) => {
            return stream_whole(writer, cursors, runtime, entry, load, replay, tally, &key)
        }
    };
    let mut file = File::open(&entry.file)?;
    let meta = file.metadata()?;
    let size = meta.len();
    let current = match if replay { None } else { cursors.get(&key)? } {
        Some(CursorRecord::Bytes(cursor)) => Some(cursor),
        Some(CursorRecord::Segment(_)) | None => None,
    };
    let prefix = match current {
        Some(cursor) => match cursor.source {
            Some(source) if cursor.offset <= size => {
                verified_prefix(&mut file, cursor.offset, source.sha256)?
            }
            _ => None,
        },
        None => Some(Sha256::new()),
    };
    let recovering = current.is_some() && prefix.is_none();
    let offset = if prefix.is_some() {
        current.map_or(0, |cursor| cursor.offset)
    } else {
        0
    };
    let mut hash = prefix.unwrap_or_default();
    if let Some(cursor) = current.filter(|_| recovering) {
        let reason = if cursor.source.is_none() {
            "legacy cursor has no verified source prefix"
        } else if size < cursor.offset {
            "source is shorter than its consumed prefix"
        } else {
            "source bytes before the cursor changed"
        };
        warn(&format!("{}: {reason}; replaying masked history without duplicating retained occurrences (previous offset {}, observed bytes {size})", entry.file.display(), cursor.offset));
        tally.replayed += 1;
        file.seek(SeekFrom::Start(0))?;
    }
    let (part_name, stem_hash) = partition_names(entry, &key)?;
    let mut retained = if recovering {
        Some(Retained::load(&writer.data_dir, runtime, &part_name)?)
    } else {
        None
    };
    let mut reader = BufReader::new(file);
    let mut batch: Vec<RawEvent> = Vec::new();
    let mut consumed = offset;
    let mut raw: Vec<u8> = Vec::new();
    loop {
        raw.clear();
        let read = reader.read_until(b'\n', &mut raw)?;
        if read == 0 || raw[read - 1] != b'\n' {
            break;
        }
        // Hash only complete consumed lines; an unfinished suffix is retried.
        hash.update(&raw[..read]);
        consumed += read as u64;
        let mut line = &raw[..read - 1];
        if line.last() == Some(&b'\r') {
            line = &line[..line.len() - 1];
        }
        batch.extend(parser.on_line(&String::from_utf8_lossy(line)));
        // A batch is what one read from the source handed over: it is written
        // and checkpointed when the reader's buffer is used up, so no event
        // count is chosen here.
        if !batch.is_empty() && reader.buffer().is_empty() {
            if recovering {
                // Keep the pre-replay cursor until the whole source succeeds.
                // A failed later batch must replay against retained history
                // again, not resume as an append and duplicate its suffix.
                writer.write_batch(
                    &batch,
                    runtime,
                    &part_name,
                    &stem_hash,
                    tally,
                    retained.as_mut(),
                )?;
                batch.clear();
                continue;
            }
            checkpoint(
                writer, cursors, &mut batch, runtime, &part_name, &stem_hash, tally, &key, &meta,
                consumed, &hash, None,
            )?;
        }
    }
    batch.extend(parser.end());
    checkpoint(
        writer,
        cursors,
        &mut batch,
        runtime,
        &part_name,
        &stem_hash,
        tally,
        &key,
        &meta,
        consumed,
        &hash,
        retained.as_mut(),
    )
}

#[allow(clippy::too_many_arguments)]
fn checkpoint(
    writer: &mut Writer,
    cursors: &mut Cursors,
    batch: &mut Vec<RawEvent>,
    runtime: &str,
    part_name: &str,
    stem_hash: &str,
    tally: &mut Tally,
    key: &str,
    meta: &fs::Metadata,
    consumed: u64,
    hash: &Sha256,
    retained: Option<&mut Retained>,
) -> Result<()> {
    if !batch.is_empty() {
        writer.write_batch(batch, runtime, part_name, stem_hash, tally, retained)?;
        batch.clear();
    }
    cursors.set_bytes(
        key,
        ByteCursor {
            mtime_ms: mtime_ms(meta),
            size: meta.len(),
            offset: consumed,
            source: Some(SourceCheckpoint::new(meta, hash.clone().finalize().into())),
        },
    );
    cursors.flush()
}
