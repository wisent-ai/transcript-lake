//! One conversation, rendered whole — to the terminal or to a file.
//!
//! `show` reconstructs a session from the canonical views. It grew a `--out`
//! path on 2026-09-11 because the operator asked for every message of a
//! session in one file, and the only way to produce one was a shell
//! redirection: the product printed, the shell saved. A redirection truncates
//! the target before the command runs, so a refusal or a lost connection left
//! an empty file behind and nobody could tell a complete record from a broken
//! one. Here the record is rendered first and written once, through a sibling
//! `.partial` file that is renamed into place, and the command says on stdout
//! what it wrote and whether the limit cut anything.
//!
//! Split out of `read.rs` because adding it there would have pushed that file
//! past the three-hundred-line limit the shared write guard enforces.

use std::fmt::Write as FmtWrite;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use super::json_i64;
use crate::args::{bounded_integer, parse_options, SHOW_LIMIT, SHOW_MAX_LIMIT};
use crate::commands::inspect::js_string;
use crate::duck::query_duck_json;
use crate::types::EVENT_TYPES;
use crate::util::{quote_sql, write_json, Error, Result};

/// The default `show` selection: the conversation itself, without tool noise.
const SHOW_DEFAULT_TYPES: [&str; 2] = ["user", "assistant"];

/// The `--include` selection for `show`: a comma-separated list of canonical
/// event types, or `all`. Duplicates collapse and the given order is kept, so
/// the `include:` header line reads back what the operator asked for.
fn include_types(raw: Option<&str>) -> Result<Vec<String>> {
    let Some(raw) = raw else {
        return Ok(SHOW_DEFAULT_TYPES
            .iter()
            .map(|kind| kind.to_string())
            .collect());
    };
    let wanted: Vec<String> = raw
        .split(',')
        .map(|part| part.trim().to_lowercase())
        .filter(|part| !part.is_empty())
        .collect();
    if wanted.is_empty() {
        return Err(Error(
            "--include needs at least one event type or \"all\"".into(),
        ));
    }
    if wanted.iter().any(|part| part == "all") {
        return Ok(EVENT_TYPES.iter().map(|kind| kind.to_string()).collect());
    }
    let mut unique: Vec<String> = Vec::with_capacity(wanted.len());
    for kind in wanted {
        if !EVENT_TYPES.contains(&kind.as_str()) {
            return Err(Error(format!(
                "unknown event type \"{kind}\" (expected one of: {}, all)",
                EVENT_TYPES.join(", ")
            )));
        }
        if !unique.contains(&kind) {
            unique.push(kind);
        }
    }
    Ok(unique)
}

/// The destination a `--out` value names, refused before any query runs so a
/// long read never ends in "no such directory".
fn destination(raw: Option<&str>) -> Result<Option<PathBuf>> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(Error("--out needs a file path".into()));
    }
    let path = PathBuf::from(trimmed);
    if path.is_dir() {
        return Err(Error(format!(
            "--out {} is a directory: name the file to write",
            path.display()
        )));
    }
    let parent = path.parent().filter(|parent| !parent.as_os_str().is_empty());
    if let Some(parent) = parent {
        if !parent.is_dir() {
            return Err(Error(format!(
                "--out directory {} does not exist",
                parent.display()
            )));
        }
    }
    Ok(Some(path))
}

/// Write the rendered record once: a sibling `.partial` file carries it and is
/// renamed into place, so the target is either the old record or the new one.
fn write_record(path: &Path, body: &str) -> Result<u64> {
    let mut partial = path.as_os_str().to_owned();
    partial.push(".partial");
    let partial = PathBuf::from(partial);
    fs::write(&partial, body).map_err(|error| {
        Error(format!(
            "cannot write {}: {error}",
            partial.display()
        ))
    })?;
    fs::rename(&partial, path).map_err(|error| {
        let _ = fs::remove_file(&partial);
        Error(format!("cannot place {}: {error}", path.display()))
    })?;
    Ok(body.len() as u64)
}

pub fn show(rest: &[String]) -> Result<i32> {
    let parsed = parse_options("show", rest, &["include", "limit", "out"], &["json"])?;
    if parsed.positionals.len() != 1 {
        return Err(Error(
            "usage: transcript-lake show <session-id> [--include <types>] [--limit <n>] \
             [--out <path>] [--json]"
                .into(),
        ));
    }
    let session_id = parsed.positionals[0].trim().to_string();
    if session_id.is_empty() {
        return Err(Error("show requires a session id".into()));
    }
    let types = include_types(parsed.value("include"))?;
    let limit = bounded_integer(parsed.value("limit"), "--limit", SHOW_LIMIT, SHOW_MAX_LIMIT)?;
    let target = destination(parsed.value("out"))?;
    let quoted_session = quote_sql(&session_id);
    let identity = query_duck_json(&format!(
        "SELECT runtime, project, first_ts, last_ts, user_msgs, assistant_msgs, tool_calls \
         FROM sessions WHERE session_id = {quoted_session}"
    ))?;
    let Some(head) = identity.first() else {
        return Err(Error(format!(
            "unknown session \"{session_id}\": not present in the selected Lake \
             (check the id or start the stream first)"
        )));
    };
    let type_filter = format!(
        " AND event_type IN ({})",
        types
            .iter()
            .map(quote_sql)
            .collect::<Vec<String>>()
            .join(", ")
    );
    // The matched count comes from its own aggregate, so a --limit cut is
    // always visible in the footer instead of silently truncating the record.
    let counted = query_duck_json(&format!(
        "SELECT count(*) AS matched FROM events WHERE session_id = {quoted_session}{type_filter}"
    ))?;
    let matched = json_i64(counted.first().and_then(|row| row.get("matched")));
    let events = query_duck_json(&format!(
        "SELECT ts, event_type, tool_name, model, coalesce(text, '') AS text FROM events \
         WHERE session_id = {quoted_session}{type_filter} ORDER BY ts LIMIT {limit}"
    ))?;
    let rendered = events.len() as i64;
    if parsed.flag("json") {
        let field = |key: &str| head.get(key).cloned().unwrap_or(Value::Null);
        let mut report = Map::new();
        report.insert("session_id".to_string(), Value::String(session_id));
        report.insert("runtime".to_string(), field("runtime"));
        report.insert("project".to_string(), field("project"));
        report.insert("first_ts".to_string(), field("first_ts"));
        report.insert("last_ts".to_string(), field("last_ts"));
        report.insert(
            "include".to_string(),
            Value::Array(types.into_iter().map(Value::String).collect()),
        );
        report.insert("matched".to_string(), Value::from(matched));
        report.insert("rendered".to_string(), Value::from(rendered));
        report.insert("events".to_string(), Value::Array(events));
        let document = Value::Object(report);
        let Some(path) = target else {
            write_json(&document)?;
            return Ok(0);
        };
        let body = format!("{}\n", serde_json::to_string_pretty(&document)?);
        let bytes = write_record(&path, &body)?;
        println!(
            "wrote {bytes} bytes to {}: {rendered} of {matched} matching events",
            path.display()
        );
        return Ok(0);
    }
    let project = match head.get("project") {
        Some(Value::Null) | None => "unknown".to_string(),
        other => js_string(other),
    };
    let mut body = String::new();
    write!(
        body,
        "session {session_id} ({})\nproject: {project}\nspan: {} .. {}\nturns: {} user, {} assistant, {} tool calls\ninclude: {}\n",
        js_string(head.get("runtime")),
        js_string(head.get("first_ts")),
        js_string(head.get("last_ts")),
        js_string(head.get("user_msgs")),
        js_string(head.get("assistant_msgs")),
        js_string(head.get("tool_calls")),
        types.join(","),
    )
    .map_err(|error| Error(error.to_string()))?;
    for event in &events {
        let event_type = js_string(event.get("event_type"));
        let tool = event.get("tool_name").and_then(Value::as_str).unwrap_or("");
        let label = if tool.is_empty() {
            event_type
        } else {
            format!("{event_type} {tool}")
        };
        write!(
            body,
            "\n[{}] {label}\n{}\n",
            js_string(event.get("ts")),
            js_string(event.get("text"))
        )
        .map_err(|error| Error(error.to_string()))?;
    }
    let suffix = if rendered < matched {
        " (raise --limit for the rest)"
    } else {
        ""
    };
    write!(
        body,
        "\nrendered {rendered} of {matched} matching events{suffix}\n"
    )
    .map_err(|error| Error(error.to_string()))?;
    let Some(path) = target else {
        print!("{body}");
        return Ok(0);
    };
    let bytes = write_record(&path, &body)?;
    println!(
        "wrote {bytes} bytes to {}: {rendered} of {matched} matching events{suffix}",
        path.display()
    );
    Ok(0)
}
