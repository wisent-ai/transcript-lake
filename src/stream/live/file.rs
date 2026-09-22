//! A byte offset is usable only after its consumed prefix has been verified.
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};

use sha2::{Digest, Sha256};

use super::super::source::Retained;
use super::super::{
    file_stem, hex_digest, warn, Tally, Writer, BATCH_EVENTS, PART_DIGEST_LEN, READ_BUFFER,
};
use crate::cursors::{ByteCursor, CursorRecord, Cursors, SourceCheckpoint};
use crate::types::{Adapter, ParserCtx, RawEvent, SessionEntry};
use crate::util::{mtime_ms, Result};

fn verified_prefix(file: &mut File, offset: u64, expected: [u8; 32]) -> Result<Option<Sha256>> {
    let mut hash = Sha256::new();
    let mut remaining = offset;
    let mut buffer = [0; READ_BUFFER];
    while remaining > 0 {
        let capacity = remaining.min(buffer.len() as u64) as usize;
        let count = file.read(&mut buffer[..capacity])?;
        if count == 0 {
            return Ok(None);
        }
        hash.update(&buffer[..count]);
        remaining -= count as u64;
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
    let digest = hex_digest(key.as_bytes());
    let part_name = format!("part-{}.ndjson", &digest[..PART_DIGEST_LEN]);
    let stem_hash = hex_digest(
        file_stem(&entry.file.file_name().unwrap_or_default().to_string_lossy()).as_bytes(),
    );
    let runtime = adapter.runtime();
    let mut retained = if recovering {
        Some(Retained::load(&writer.data_dir, runtime, &part_name)?)
    } else {
        None
    };
    let mut parser = adapter.parser(ParserCtx {
        file: entry.file.clone(),
        session_id: entry.session_id.clone(),
        project: entry.project.clone(),
    });
    let mut reader = BufReader::with_capacity(READ_BUFFER, file);
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
        if batch.len() >= BATCH_EVENTS {
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
