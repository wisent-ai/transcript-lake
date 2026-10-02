//! Operator labels over Lake sessions: two append-only writes — an
//! assignment and its withdrawal — and two reads that collapse the append
//! log to the latest assignment per session and aspect. A write is refused
//! for a session the selected Lake does not hold, so a typo cannot create a
//! label nothing can ever join against.
use serde_json::Value;

use crate::args::{
    bounded_integer, parse_options, require_flags_only, require_runtime, DEFAULT_LIMIT, MAX_LIMIT,
};
use crate::duck::{query_duck_json, run_duck_query};
use crate::labels::{
    append_label, label_record, normalize_aspect, normalize_label_value, normalize_note,
    normalize_source, withdrawal_record,
};
use crate::paths::resolve_data_dir;
use crate::util::{quote_sql, write_json, Error, Result};

const ADD_USAGE: &str = concat!(
    "usage: transcript-lake label add <session-id> --aspect <name> --value <v>",
    " [--note <text>] [--source <name[:detail]>] [--json]",
);
const REMOVE_USAGE: &str = concat!(
    "usage: transcript-lake label remove <session-id> --aspect <name>",
    " [--note <text>] [--source <name[:detail]>] [--runtime <r>] [--json]",
);

pub fn label(rest: &[String]) -> Result<i32> {
    match rest.split_first() {
        Some((subcommand, subrest)) if subcommand == "add" => add(subrest),
        Some((subcommand, subrest)) if subcommand == "remove" => remove(subrest),
        Some((subcommand, subrest)) if subcommand == "list" => list(subrest),
        Some((subcommand, subrest)) if subcommand == "aspects" => aspects(subrest),
        _ => Err(Error(
            "usage: transcript-lake label <add|remove|list|aspects> (see: transcript-lake help label)"
                .into(),
        )),
    }
}

fn add(rest: &[String]) -> Result<i32> {
    let parsed = parse_options(
        "label add",
        rest,
        &["aspect", "value", "note", "runtime", "source"],
        &["json"],
    )?;
    if parsed.positionals.len() != 1 {
        return Err(Error(ADD_USAGE.into()));
    }
    let session_id = parsed.positionals[0].trim().to_string();
    if session_id.is_empty() {
        return Err(Error("label add requires a session id".into()));
    }
    let aspect = normalize_aspect(parsed.value("aspect"))?;
    let value = normalize_label_value(parsed.value("value"))?;
    let note = normalize_note(parsed.value("note"));
    let source = normalize_source(parsed.value("source"))?;
    let runtime = session_runtime(&session_id, parsed.value("runtime"))?;
    let record = label_record(&session_id, &runtime, &aspect, &value, note, &source);
    append_label(&resolve_data_dir(None), &record)?;
    if parsed.flag("json") {
        write_json(&record)?;
        return Ok(0);
    }
    let note = match &record.note {
        Some(note) => format!(" (note: {note})"),
        None => String::new(),
    };
    println!(
        "labeled {} ({}): {} = {}{note}",
        record.session_id,
        record.runtime,
        record.aspect,
        record.value.as_deref().unwrap_or_default()
    );
    Ok(0)
}

/// `label remove`: withdraw the assignment in force for one session and
/// aspect by appending a record with no value. A session with no assignment
/// for that aspect is refused, so a second withdrawal never pretends to
/// take something back twice.
fn remove(rest: &[String]) -> Result<i32> {
    let parsed = parse_options(
        "label remove",
        rest,
        &["aspect", "note", "runtime", "source"],
        &["json"],
    )?;
    if parsed.positionals.len() != 1 {
        return Err(Error(REMOVE_USAGE.into()));
    }
    let session_id = parsed.positionals[0].trim().to_string();
    if session_id.is_empty() {
        return Err(Error("label remove requires a session id".into()));
    }
    let aspect = normalize_aspect(parsed.value("aspect"))?;
    let note = normalize_note(parsed.value("note"));
    let source = normalize_source(parsed.value("source"))?;
    let runtime = session_runtime(&session_id, parsed.value("runtime"))?;
    let in_force = query_duck_json(&format!(
        "SELECT value FROM ({}) WHERE rn = CAST('1' AS BIGINT) AND value IS NOT NULL",
        latest_labels_inner(&[
            format!("session_id = {}", quote_sql(&session_id)),
            format!("aspect = {}", quote_sql(&aspect)),
        ])
    ))?;
    if in_force.is_empty() {
        return Err(Error(format!(
            "session \"{session_id}\" has no {aspect} label in force; nothing to remove"
        )));
    }
    let record = withdrawal_record(&session_id, &runtime, &aspect, note, &source);
    append_label(&resolve_data_dir(None), &record)?;
    if parsed.flag("json") {
        write_json(&record)?;
        return Ok(0);
    }
    println!(
        "removed the {} label of {} ({})",
        record.aspect, record.session_id, record.runtime
    );
    Ok(0)
}

/// The runtime a session is held under in the selected Lake. The session
/// must already exist: a label that joins to nothing is indistinguishable
/// from a mistyped id. `--runtime` disambiguates an id held under several.
fn session_runtime(session_id: &str, requested: Option<&str>) -> Result<String> {
    let rows = query_duck_json(&format!(
        "SELECT DISTINCT runtime FROM sessions WHERE session_id = {}",
        quote_sql(session_id)
    ))?;
    if rows.is_empty() {
        return Err(Error(format!(
            "unknown session \"{session_id}\": not present in the selected Lake{}",
            " (check the id or start the stream first)"
        )));
    }
    let mut runtimes: Vec<String> = rows
        .iter()
        .map(|row| match row.get("runtime") {
            Some(Value::String(text)) => text.clone(),
            Some(other) => other.to_string(),
            None => String::new(),
        })
        .collect();
    runtimes.sort_unstable();
    if let Some(selected) = require_runtime(requested)? {
        if !runtimes.iter().any(|known| known == &selected) {
            return Err(Error(format!(
                "session \"{session_id}\" exists under {}, not {selected}",
                runtimes.join(", ")
            )));
        }
        return Ok(selected);
    }
    if runtimes.len() > 1 {
        return Err(Error(format!(
            "session id \"{session_id}\" is ambiguous across runtimes ({}); repeat with --runtime",
            runtimes.join(", ")
        )));
    }
    Ok(runtimes.remove(0))
}

/// The append log ranked per session and aspect, newest first, so the outer
/// query keeps only the assignment currently in force.
fn latest_labels_inner(where_clauses: &[String]) -> String {
    let mut sql = "SELECT ts, session_id, runtime, aspect, value, note, source, \
                   row_number() OVER (PARTITION BY session_id, aspect ORDER BY ts DESC) AS rn \
                   FROM labels"
        .to_string();
    if !where_clauses.is_empty() {
        sql.push_str(" WHERE ");
        sql.push_str(&where_clauses.join(" AND "));
    }
    sql
}

fn list(rest: &[String]) -> Result<i32> {
    let parsed = parse_options(
        "label list",
        rest,
        &["session", "aspect", "runtime", "limit"],
        &["json"],
    )?;
    require_flags_only("label list", &parsed)?;
    let limit = bounded_integer(parsed.value("limit"), "--limit", DEFAULT_LIMIT, MAX_LIMIT)?;
    let mut where_clauses = Vec::new();
    if let Some(session) = parsed.value("session") {
        where_clauses.push(format!("session_id = {}", quote_sql(session)));
    }
    if let Some(aspect) = parsed.value("aspect") {
        where_clauses.push(format!(
            "aspect = {}",
            quote_sql(normalize_aspect(Some(aspect))?)
        ));
    }
    if let Some(runtime) = require_runtime(parsed.value("runtime"))? {
        where_clauses.push(format!("runtime = {}", quote_sql(runtime)));
    }
    run_duck_query(
        &format!(
            "SELECT ts, session_id, runtime, aspect, value, note, source FROM ({}) \
             WHERE rn = CAST('1' AS BIGINT) AND value IS NOT NULL ORDER BY ts DESC LIMIT {limit}",
            latest_labels_inner(&where_clauses)
        ),
        parsed.flag("json"),
        false,
    )
}

fn aspects(rest: &[String]) -> Result<i32> {
    let parsed = parse_options("label aspects", rest, &[], &["json"])?;
    require_flags_only("label aspects", &parsed)?;
    run_duck_query(
        &format!(
            "SELECT aspect, count(DISTINCT value) AS values, count(*) AS labels, \
             count(DISTINCT session_id) AS sessions FROM ({}) \
             WHERE rn = CAST('1' AS BIGINT) AND value IS NOT NULL GROUP BY aspect ORDER BY labels DESC, aspect",
            latest_labels_inner(&[])
        ),
        parsed.flag("json"),
        false,
    )
}
