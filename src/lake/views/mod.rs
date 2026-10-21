//! Which canonical views one statement binds.
//!
//! DuckDB binds a view when it is created, and every view over `events`
//! binds `events` again: its multi-file read over every event part file of
//! the Lake. With tens of thousands of part files each such binding costs
//! seconds to minutes of file-system reads, and the canonical script bound
//! `events` about nine times before the caller's statement ran, so even
//! `select 1` waited minutes for its first row. A statement now binds only
//! the views it names and the views those are written over.
//!
//! `views.sql` is divided into sections, each opened by a line
//! `-- view: <name> [after <view> ...]`: the statements that create the view
//! `<name>`, written over the views named after `after`. The statement's
//! own references are read from DuckDB's parser (`json_serialize_sql`), the
//! same parser that runs it, never from its text. A statement the parser
//! does not serialize (a DESCRIBE, a PRAGMA) binds every view, as before,
//! and says so on standard error.

use std::collections::BTreeSet;
use std::process::Command;

use serde_json::Value;

use crate::util::{quote_sql, Error, Result};

/// The line that opens a section of `views.sql`.
const MARKER: &str = "-- view: ";
/// The word between a section's view and the views it is written over.
const AFTER: &str = "after";

/// One section of the view script: the view it creates, the views it is
/// written over, and its statements.
struct Section<'a> {
    name: &'a str,
    after: Vec<&'a str>,
    text: String,
}

/// The script's text before its first section, and its sections in order.
fn sections(script: &str) -> Result<(String, Vec<Section<'_>>)> {
    let mut preamble = String::new();
    let mut found: Vec<Section<'_>> = Vec::new();
    for line in script.lines() {
        if let Some(declared) = line.strip_prefix(MARKER) {
            let mut words = declared.split_whitespace();
            let name = words
                .next()
                .ok_or_else(|| Error(format!("views.sql section line {line:?} names no view")))?;
            let after: Vec<&str> = match words.next() {
                None => Vec::new(),
                Some(word) if word == AFTER => words.collect(),
                Some(word) => {
                    return Err(Error(format!(
                        "views.sql section line {line:?} reads {word:?} where {AFTER:?} or the line's end belongs"
                    )));
                }
            };
            found.push(Section {
                name,
                after,
                text: String::new(),
            });
        }
        let into = match found.last_mut() {
            Some(section) => &mut section.text,
            None => &mut preamble,
        };
        into.push_str(line);
        into.push('\n');
    }
    for section in &found {
        for over in &section.after {
            if !found.iter().any(|other| other.name == *over) {
                return Err(Error(format!(
                    "views.sql section {} is written over {over}, which no section creates",
                    section.name
                )));
            }
        }
    }
    Ok((preamble, found))
}

/// Every table name the statement references, as DuckDB's parser reads it,
/// or the parser's reason when it does not serialize the statement.
fn referenced(sql: &str) -> Result<std::result::Result<BTreeSet<String>, String>> {
    let parse = format!("SELECT json_serialize_sql({}) AS ast", quote_sql(sql));
    let mut command = Command::new("duckdb");
    command.args(["-json", "-c", &parse]);
    let output = stado_wait::output(&mut command).map_err(unstarted)?;
    if !output.status.success() {
        return Err(Error(format!(
            "duckdb parsing the statement ended with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let rows: Value = serde_json::from_slice(&output.stdout)?;
    let ast = rows
        .as_array()
        .and_then(|rows| rows.first())
        .map(|row| &row["ast"])
        .ok_or_else(|| Error(format!("duckdb answered no parse of the statement: {rows}")))?;
    let ast: Value = match ast {
        Value::String(text) => serde_json::from_str(text)?,
        other => other.clone(),
    };
    if ast["error"] == Value::Bool(true) {
        return Ok(Err(match ast["error_message"].as_str() {
            Some(message) => message.to_string(),
            None => format!("DuckDB's parser did not serialize it: {ast}"),
        }));
    }
    let mut names = BTreeSet::new();
    collect(&ast, &mut names);
    Ok(Ok(names))
}

/// DuckDB could not be started to parse the statement.
fn unstarted(error: impl std::fmt::Display) -> Error {
    Error(format!("duckdb failed to start to parse the statement: {error}"))
}

/// The `table_name` of every base-table reference in the parse tree.
fn collect(node: &Value, names: &mut BTreeSet<String>) {
    match node {
        Value::Object(fields) => {
            if fields.get("type").and_then(Value::as_str) == Some("BASE_TABLE") {
                if let Some(name) = fields.get("table_name").and_then(Value::as_str) {
                    names.insert(name.to_lowercase());
                }
            }
            for value in fields.values() {
                collect(value, names);
            }
        }
        Value::Array(items) => items.iter().for_each(|item| collect(item, names)),
        _ => {}
    }
}

/// The view script `sql` needs: the preamble and, in the script's order, the
/// sections of the views it references and of the views those are written
/// over. With `every` set, or for a statement the parser does not serialize,
/// every section.
pub fn needed(script: &str, sql: &str, every: bool) -> Result<String> {
    let (preamble, sections) = sections(script)?;
    let every_view: Vec<&str> = sections.iter().map(|section| section.name).collect();
    let wanted: Vec<&str> = if every {
        every_view
    } else {
        match referenced(sql)? {
            Ok(names) => every_view
                .into_iter()
                .filter(|name| names.contains(&name.to_lowercase()))
                .collect(),
            Err(reason) => {
                eprintln!(
                    "transcript-lake: DuckDB's parser does not read this statement's references ({reason}); every view is bound"
                );
                every_view
            }
        }
    };
    let mut closure: BTreeSet<&str> = BTreeSet::new();
    let mut pending = wanted;
    while let Some(name) = pending.pop() {
        if closure.insert(name) {
            if let Some(section) = sections.iter().find(|section| section.name == name) {
                pending.extend(section.after.iter().copied());
            }
        }
    }
    let mut chosen = preamble;
    for kept in sections.iter().filter(|kept| closure.contains(kept.name)) {
        chosen.push_str(&kept.text);
    }
    Ok(chosen)
}
