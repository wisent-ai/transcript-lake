//! Masking, canonical partition commits and Oko projection publication.
use super::source::Retained;
use super::{file_stem, hex_digest, Tally, EXTRA_DEPTH, TEXT_CAP};
use crate::redact::Masker;
use crate::types::{CanonicalEvent, EventSink, RawEvent, SegmentOutput, HOOKS};
use crate::util::{Error, Result};
use serde_json::{Map, Value};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
/// Cut to the text cap counting UTF-16 units, which is what the previous
/// implementation's `String.length` counted. A byte length within the cap can
/// never exceed it in UTF-16 units, so the common case never scans the string.
///
/// One deliberate deviation, the only one measured across the ported corpus:
/// when the cap lands mid astral character, `slice` left a lone high surrogate
/// behind and serialized it as `\ud83d`, which is not valid Unicode and which
/// a Rust `String` cannot hold. The character is kept out instead, so such a
/// line is one character shorter than the previous implementation wrote.
fn clip(mut text: String) -> String {
    if text.len() <= TEXT_CAP {
        return text;
    }
    let mut units = 0usize;
    let mut end = text.len();
    for (index, character) in text.char_indices() {
        let width = character.len_utf16();
        if units + width > TEXT_CAP {
            end = index;
            break;
        }
        units += width;
    }
    text.truncate(end);
    text
}

/// The date partition an event belongs to: the leading `YYYY-MM-DD` of its
/// timestamp. An unusable timestamp lands in a visible catch-all partition,
/// not dropped.
fn date_of(ts: Option<&str>) -> String {
    let Some(ts) = ts else {
        return "unknown".to_string();
    };
    let bytes = ts.as_bytes();
    let shaped = bytes.len() >= 10
        && bytes[..4].iter().all(u8::is_ascii_digit)
        && bytes[4] == b'-'
        && bytes[5..7].iter().all(u8::is_ascii_digit)
        && bytes[7] == b'-'
        && bytes[8..10].iter().all(u8::is_ascii_digit);
    if shaped {
        ts[..10].to_string()
    } else {
        "unknown".to_string()
    }
}

/// Masks every string inside extra, to a small depth bound (extra stays
/// small). Non-string leaves pass through untouched; JSON serialization later
/// renders them exactly as the adapter emitted them.
fn mask_deep(value: Value, masker: &mut Masker, depth: i32) -> Value {
    match value {
        Value::String(text) => Value::String(clip(masker.mask(&text))),
        Value::Array(items) => {
            if depth <= 0 {
                return Value::Null;
            }
            Value::Array(
                items
                    .into_iter()
                    .map(|item| mask_deep(item, masker, depth - 1))
                    .collect(),
            )
        }
        Value::Object(fields) => {
            if depth <= 0 {
                return Value::Null;
            }
            Value::Object(
                fields
                    .into_iter()
                    .map(|(key, item)| (key, mask_deep(item, masker, depth - 1)))
                    .collect(),
            )
        }
        other => other,
    }
}

/// The single masking boundary: every canonical event in the Lake is produced
/// here, so no adapter can route unmasked text to a partition.
pub struct Writer {
    pub(super) data_dir: PathBuf,
    machine: String,
    pub(super) masker: Masker,
}

impl Writer {
    pub(super) fn new(data_dir: PathBuf, machine: String) -> Self {
        Self {
            data_dir,
            machine,
            masker: Masker::new(),
        }
    }

    fn canonicalize(&mut self, event: &RawEvent, runtime: &str) -> (CanonicalEvent, String) {
        let date = date_of(event.ts.as_deref());
        let text = clip(self.masker.mask(&event.text));
        let extra = mask_deep(
            Value::Object(event.extra.clone()),
            &mut self.masker,
            EXTRA_DEPTH,
        );
        let canonical = CanonicalEvent {
            ts: event.ts.clone(),
            runtime: runtime.to_string(),
            machine: self.machine.clone(),
            session_id: event.session_id.clone(),
            project: event.project.clone(),
            event_type: if event.event_type.is_empty() {
                "meta".to_string()
            } else {
                event.event_type.clone()
            },
            text,
            tool_name: event.tool_name.clone(),
            model: event.model.clone(),
            tokens_in: event.tokens_in,
            tokens_out: event.tokens_out,
            // A depth bound can null the whole map only when extra is nested
            // deeper than the bound, which the object itself never is.
            extra: match extra {
                Value::Object(fields) => fields,
                _ => Map::new(),
            },
        };
        (canonical, date)
    }

    /// Persist one source delta to canonical partitions and Oko's live
    /// per-session projection before the source cursor can advance.
    pub(super) fn write_batch(
        &mut self,
        events: &[RawEvent],
        runtime: &str,
        part_name: &str,
        stem_hash: &str,
        tally: &mut Tally,
        mut retained: Option<&mut Retained>,
    ) -> Result<()> {
        let mut rows: Vec<(PathBuf, String)> = Vec::with_capacity(events.len());
        let mut projections: Vec<Value> = Vec::with_capacity(events.len());
        for event in events {
            let (mut canonical, date) = self.canonicalize(event, runtime);
            canonical.extra.insert(
                "source_stem_hash".to_string(),
                Value::String(stem_hash.to_string()),
            );
            let projected = serde_json::to_value(canonical)?;
            let present = retained
                .as_mut()
                .is_some_and(|known| known.already_recorded(&projected, runtime));
            if !present {
                let dir = self
                    .data_dir
                    .join("events")
                    .join(format!("runtime={runtime}"))
                    .join(format!("date={date}"));
                rows.push((dir, serde_json::to_string(&projected)?));
                tally.events += 1;
            }
            // A replay also restores a missing derived row without writing
            // its already durable canonical occurrence again.
            projections.push(projected);
        }
        let mut dirs: Vec<&PathBuf> = Vec::new();
        for (dir, _) in &rows {
            if !dirs.contains(&dir) {
                dirs.push(dir);
            }
        }
        for dir in dirs {
            let partition = dir.join(part_name);
            let failure = |error| {
                Error(format!(
                    "cannot commit canonical partition {}: {error}",
                    partition.display()
                ))
            };
            fs::create_dir_all(dir).map_err(failure)?;
            let mut payload = String::new();
            for (row_dir, line) in &rows {
                if row_dir == dir {
                    payload.push_str(line);
                    payload.push('\n');
                }
            }
            let mut file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&partition)
                .map_err(failure)?;
            file.write_all(payload.as_bytes()).map_err(failure)?;
            file.sync_all().map_err(failure)?;
        }
        crate::oko_export::project_events(&self.data_dir, projections)?;
        Ok(())
    }
}

fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)?.sync_all()?;
    Ok(())
}

/// Whole-file publish for immutable output: unique temp, sync, rename, then
/// parent sync, so a reader never observes a partial segment partition.
fn durable_write(path: &Path, content: &str) -> Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let name = path
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let temporary = parent.join(format!(".{name}.{}.tmp", uuid::Uuid::new_v4()));
    let write = (|| -> Result<()> {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(content.as_bytes())?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, path)?;
        sync_directory(parent)
    })();
    if write.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    write
}

/// Closed segments are immutable and content-addressed: republishing the same
/// bytes is a no-op, republishing different bytes under the same name is a
/// refusal, never an overwrite.
impl EventSink for Writer {
    fn accept(&mut self, source_file: &Path, events: &[RawEvent]) -> Result<Vec<SegmentOutput>> {
        let stem = source_file
            .file_name()
            .map(|name| file_stem(&name.to_string_lossy()))
            .unwrap_or_default();
        let part_name = format!("{stem}.ndjson");
        // First-appearance order, exactly like the Map the previous
        // implementation grouped into.
        let mut groups: Vec<(String, Vec<String>)> = Vec::new();
        for event in events {
            let (canonical, date) = self.canonicalize(event, HOOKS);
            let line = serde_json::to_string(&canonical)?;
            match groups.iter_mut().find(|(known, _)| *known == date) {
                Some((_, lines)) => lines.push(line),
                None => groups.push((date, vec![line])),
            }
        }
        let mut outputs = Vec::with_capacity(groups.len());
        for (date, lines) in groups {
            let mut content = lines.join("\n");
            content.push('\n');
            let sha256 = hex_digest(content.as_bytes());
            let path = self
                .data_dir
                .join("events")
                .join(format!("runtime={HOOKS}"))
                .join(format!("date={date}"))
                .join(&part_name);
            if path.exists() {
                if hex_digest(&fs::read(&path)?) != sha256 {
                    return Err(Error(format!(
                        "hook segment output conflict: {}",
                        path.display()
                    )));
                }
            } else {
                durable_write(&path, &content)?;
            }
            outputs.push(SegmentOutput { path, sha256 });
        }
        Ok(outputs)
    }
}
