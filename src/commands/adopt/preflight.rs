//! What is checked before a source is adopted: that the root really is a
//! supported one for this runtime, and that its files parse into the records
//! the runtime is contracted to produce, counted rather than guessed.

use std::fs::{self, File};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::types::{Adapter, ParserCtx};
use crate::util::{home_dir, Error, Result};

use super::Preflight;


pub(super) fn canonical_supported_root(adapter: &dyn Adapter, requested: &Path) -> Result<PathBuf> {
    let root = fs::canonicalize(requested).map_err(|error| {
        Error(format!(
            "could not resolve source root {}: {error}",
            requested.display()
        ))
    })?;
    if !root.is_dir() {
        return Err(Error(format!(
            "source root is not a directory: {}",
            root.display()
        )));
    }
    let discovered: Vec<PathBuf> = adapter
        .roots(&home_dir())
        .into_iter()
        .filter_map(|path| fs::canonicalize(path).ok())
        .collect();
    if !discovered.contains(&root) {
        let expected = if discovered.is_empty() {
            "none discovered".to_string()
        } else {
            discovered
                .iter()
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        };
        return Err(Error(format!(
            "{} is not a discovered {} transcript root (discovered: {expected})",
            root.display(),
            adapter.runtime()
        )));
    }
    Ok(root)
}

pub(super) fn preflight(adapter: &dyn Adapter, root: &Path) -> Result<Preflight> {
    let entries = adapter.list_sessions(root);
    if entries.is_empty() {
        return Err(Error(format!(
            "source root {} contains no supported {} transcript files",
            root.display(),
            adapter.runtime()
        )));
    }
    let mut events = 0u64;
    let mut pending_lines = 0u64;
    for entry in &entries {
        let canonical = fs::canonicalize(&entry.file).map_err(|error| {
            Error(format!(
                "could not resolve transcript {}: {error}",
                entry.file.display()
            ))
        })?;
        if !canonical.starts_with(root) || !fs::metadata(&canonical)?.is_file() {
            return Err(Error(format!(
                "transcript escaped its selected source root: {}",
                entry.file.display()
            )));
        }
        if adapter.entry_for(&entry.file).is_none() {
            return Err(Error(format!(
                "adapter {} does not accept discovered transcript {}",
                adapter.runtime(),
                entry.file.display()
            )));
        }
        let mut parser = adapter.parser(ParserCtx {
            file: entry.file.clone(),
            session_id: entry.session_id.clone(),
            project: entry.project.clone(),
        });
        if entry.file.to_string_lossy().ends_with(".settings.json") {
            let body = fs::read_to_string(&entry.file)?;
            let value: Value = serde_json::from_str(&body).map_err(|error| {
                Error(format!(
                    "invalid Factory settings JSON in {}: {error}; no source data was ingested",
                    entry.file.display()
                ))
            })?;
            if !value.is_object() {
                return Err(Error(format!(
                    "Factory settings file {} is not a JSON object; no source data was ingested",
                    entry.file.display()
                )));
            }
            let mut accepted = 0u64;
            for line in body.lines() {
                accepted += parser.on_line(line).len() as u64;
            }
            accepted += parser.end().len() as u64;
            if accepted == 0 {
                return Err(Error(format!(
                    "Factory settings file {} uses no supported settings fields; no source data was ingested",
                    entry.file.display()
                )));
            }
            events += accepted;
            continue;
        }
        let mut reader = BufReader::new(File::open(&entry.file)?);
        let mut line = Vec::new();
        let mut line_number = 0u64;
        loop {
            line.clear();
            let read = reader.read_until(b'\n', &mut line)?;
            if read == 0 {
                break;
            }
            line_number += 1;
            if line.last() != Some(&b'\n') {
                pending_lines += 1;
                break;
            }
            let mut body = &line[..line.len() - 1];
            if body.last() == Some(&b'\r') {
                body = &body[..body.len() - 1];
            }
            if body.iter().all(u8::is_ascii_whitespace) {
                continue;
            }
            let record = serde_json::from_slice::<Value>(body).map_err(|error| {
                Error(format!(
                    "invalid complete JSON record in {}: {error}; no source data was ingested",
                    entry.file.display()
                ))
            })?;
            validate_supported_record(adapter.runtime(), &record, &entry.file, line_number)?;
            events += parser.on_line(&String::from_utf8_lossy(body)).len() as u64;
        }
        events += parser.end().len() as u64;
    }
    if events == 0 {
        return Err(Error(format!(
            "source root {} contains transcript files but no records accepted by the {} adapter",
            root.display(),
            adapter.runtime()
        )));
    }
    Ok(Preflight {
        files: entries.len() as u64,
        events,
        pending_lines,
    })
}

fn validate_supported_record(
    runtime: &str,
    record: &Value,
    file: &Path,
    line: u64,
) -> Result<()> {
    if !record.is_object() {
        return Err(Error(format!(
            "unsupported non-object JSON record in {} at line {line}; no source data was ingested",
            file.display()
        )));
    }
    let Some(kind) = record.get("type").and_then(Value::as_str) else {
        if runtime == "omp" || runtime == "droid" {
            return Ok(());
        }
        return Err(Error(format!(
            "supported {runtime} JSONL requires a string type in {} at line {line}; no source data was ingested",
            file.display()
        )));
    };
    let supported = match runtime {
        "claude" => matches!(
            kind,
            "user"
                | "assistant"
                | "system"
                | "summary"
                | "progress"
                | "file-history-snapshot"
                | "queue-operation"
        ),
        "codex" => matches!(
            kind,
            "session_meta" | "turn_context" | "event_msg" | "response_item" | "compacted"
        ),
        "kimi" => matches!(
            kind,
            "context.append_loop_event"
                | "context.append_message"
                | "usage.record"
                | "metadata"
                | "config.update"
                | "context.apply_compaction"
                | "turn.cancel"
                | "turn.prompt"
                | "turn.steer"
        ) || kind.starts_with("tools.")
            || kind.starts_with("permission.")
            || kind.contains("_mode."),
        // OMP and Factory Droid preserve otherwise unknown record kinds as
        // explicit metadata events rather than dropping them.
        "omp" | "droid" => true,
        _ => false,
    };
    if supported {
        return Ok(());
    }
    Err(Error(format!(
        "unsupported {runtime} record type '{kind}' in {} at line {line}; no source data was ingested",
        file.display()
    )))
}
