//! Read-only analytics over the canonical DuckDB views. Every command here
//! builds one statement and hands it to `crate::duck`, which loads the views
//! first; DuckDB renders its own table or `-json` output, so the exit status
//! of the child is the exit status of the command. `show` is the exception: it
//! reads rows back as JSON because it reconstructs a whole conversation and
//! reports how much of it the limit cut.
use std::io::Write;

use serde_json::{Map, Value};

use crate::args::{
    bounded_integer, parse_options, require_flags_only, require_runtime, DEFAULT_DAYS,
    DEFAULT_LIMIT, MAX_LIMIT, SHOW_LIMIT, SHOW_MAX_LIMIT,
};
use crate::duck::{query_duck_json, run_duck_query};
use crate::types::EVENT_TYPES;
use crate::util::{quote_sql, write_json, Error, Result};

use super::inspect::js_string;

/// The default `show` selection: the conversation itself, without tool noise.
const SHOW_DEFAULT_TYPES: [&str; 2] = ["user", "assistant"];

/// `WHERE a AND b`, or nothing at all when no filter was requested.
fn where_clause(conditions: &[String]) -> String {
    if conditions.is_empty() {
        return String::new();
    }
    format!(" WHERE {}", conditions.join(" AND "))
}

/// A JSON field as a whole number, tolerating the string spelling DuckDB uses
/// for values that do not fit a JSON number.
fn json_i64(value: Option<&Value>) -> i64 {
    match value {
        Some(Value::Number(number)) => number
            .as_i64()
            .unwrap_or_else(|| number.as_f64().unwrap_or(0.0) as i64),
        Some(Value::String(text)) => text.trim().parse().unwrap_or(0),
        _ => 0,
    }
}

pub fn sessions(rest: &[String]) -> Result<i32> {
    let parsed = parse_options(
        "sessions",
        rest,
        &["runtime", "project", "limit"],
        &["json", "interrupted"],
    )?;
    require_flags_only("sessions", &parsed)?;
    let runtime = require_runtime(parsed.value("runtime"))?;
    let limit = bounded_integer(parsed.value("limit"), "--limit", DEFAULT_LIMIT, MAX_LIMIT)?;
    let mut conditions = Vec::new();
    if let Some(runtime) = runtime {
        conditions.push(format!("runtime = {}", quote_sql(runtime)));
    }
    if let Some(project) = parsed.value("project") {
        conditions.push(format!(
            "lower(coalesce(project, '')) LIKE lower({})",
            quote_sql(format!("%{project}%"))
        ));
    }
    // The interrupted view carries the diagnosis (stopped_as, the opening of
    // the unanswered request) in place of the token counters, which say
    // nothing about why a conversation stopped.
    let (view, columns) = if parsed.flag("interrupted") {
        (
            "interrupted_sessions",
            "runtime, session_id, project, stopped_as, first_ts, last_ts, user_msgs, \
             assistant_msgs, tool_calls, last_user_text",
        )
    } else {
        (
            "sessions",
            "runtime, session_id, project, first_ts, last_ts, user_msgs, assistant_msgs, \
             tool_calls, tokens_in, tokens_out",
        )
    };
    run_duck_query(
        &format!(
            "SELECT {columns} FROM {view}{} ORDER BY last_ts DESC LIMIT {limit}",
            where_clause(&conditions)
        ),
        parsed.flag("json"),
        false,
    )
}

pub fn events(rest: &[String]) -> Result<i32> {
    let parsed = parse_options(
        "events",
        rest,
        &["runtime", "session", "type", "limit"],
        &["json"],
    )?;
    require_flags_only("events", &parsed)?;
    let runtime = require_runtime(parsed.value("runtime"))?;
    let limit = bounded_integer(parsed.value("limit"), "--limit", DEFAULT_LIMIT, MAX_LIMIT)?;
    let mut conditions = Vec::new();
    if let Some(runtime) = runtime {
        conditions.push(format!("runtime = {}", quote_sql(runtime)));
    }
    if let Some(session) = parsed.value("session") {
        conditions.push(format!("session_id = {}", quote_sql(session)));
    }
    if let Some(event_type) = parsed.value("type") {
        conditions.push(format!("event_type = {}", quote_sql(event_type)));
    }
    run_duck_query(
        &format!(
            "SELECT ts, runtime, session_id, project, event_type, tool_name, model, tokens_in, \
             tokens_out, substr(text, CAST('1' AS INTEGER), CAST('240' AS INTEGER)) AS text \
             FROM events{} ORDER BY ts DESC LIMIT {limit}",
            where_clause(&conditions)
        ),
        parsed.flag("json"),
        false,
    )
}

pub fn search(rest: &[String]) -> Result<i32> {
    let parsed = parse_options(
        "search",
        rest,
        &["runtime", "session", "type", "limit"],
        &["json"],
    )?;
    let term = parsed.positionals.join(" ").trim().to_string();
    if term.is_empty() {
        return Err(Error(
            "usage: transcript-lake search [--json] <text>".into(),
        ));
    }
    let runtime = require_runtime(parsed.value("runtime"))?;
    let limit = bounded_integer(parsed.value("limit"), "--limit", DEFAULT_LIMIT, MAX_LIMIT)?;
    // The operator typed a literal, not a pattern: neutralise the LIKE
    // wildcards with an explicit escape character so `100%` finds `100%`.
    let literal = term
        .replace('!', "!!")
        .replace('%', "!%")
        .replace('_', "!_");
    let mut conditions = vec![format!(
        "lower(text) LIKE lower({}) ESCAPE '!'",
        quote_sql(format!("%{literal}%"))
    )];
    if let Some(runtime) = runtime {
        conditions.push(format!("runtime = {}", quote_sql(runtime)));
    }
    if let Some(session) = parsed.value("session") {
        conditions.push(format!("session_id = {}", quote_sql(session)));
    }
    if let Some(event_type) = parsed.value("type") {
        conditions.push(format!("event_type = {}", quote_sql(event_type)));
    }
    run_duck_query(
        &format!(
            "SELECT ts, runtime, session_id, event_type, \
             substr(text, CAST('1' AS INTEGER), CAST('240' AS INTEGER)) AS text FROM events \
             WHERE {} ORDER BY ts DESC LIMIT {limit}",
            conditions.join(" AND ")
        ),
        parsed.flag("json"),
        false,
    )
}

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

pub fn show(rest: &[String]) -> Result<i32> {
    let parsed = parse_options("show", rest, &["include", "limit"], &["json"])?;
    if parsed.positionals.len() != 1 {
        return Err(Error(
            "usage: transcript-lake show <session-id> [--include <types>] [--limit <n>] [--json]"
                .into(),
        ));
    }
    let session_id = parsed.positionals[0].trim().to_string();
    if session_id.is_empty() {
        return Err(Error("show requires a session id".into()));
    }
    let types = include_types(parsed.value("include"))?;
    let limit = bounded_integer(parsed.value("limit"), "--limit", SHOW_LIMIT, SHOW_MAX_LIMIT)?;
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
        write_json(&Value::Object(report))?;
        return Ok(0);
    }
    let mut out = std::io::stdout().lock();
    let project = match head.get("project") {
        Some(Value::Null) | None => "unknown".to_string(),
        other => js_string(other),
    };
    write!(
        out,
        "session {session_id} ({})\nproject: {project}\nspan: {} .. {}\nturns: {} user, {} assistant, {} tool calls\ninclude: {}\n",
        js_string(head.get("runtime")),
        js_string(head.get("first_ts")),
        js_string(head.get("last_ts")),
        js_string(head.get("user_msgs")),
        js_string(head.get("assistant_msgs")),
        js_string(head.get("tool_calls")),
        types.join(","),
    )?;
    for event in &events {
        let event_type = js_string(event.get("event_type"));
        let tool = event.get("tool_name").and_then(Value::as_str).unwrap_or("");
        let label = if tool.is_empty() {
            event_type
        } else {
            format!("{event_type} {tool}")
        };
        write!(
            out,
            "\n[{}] {label}\n{}\n",
            js_string(event.get("ts")),
            js_string(event.get("text"))
        )?;
    }
    let suffix = if rendered < matched {
        " (raise --limit for the rest)"
    } else {
        ""
    };
    write!(
        out,
        "\nrendered {rendered} of {matched} matching events{suffix}\n"
    )?;
    Ok(0)
}

pub fn stats(rest: &[String]) -> Result<i32> {
    let parsed = parse_options("stats", rest, &["days", "runtime"], &["json"])?;
    require_flags_only("stats", &parsed)?;
    let runtime = require_runtime(parsed.value("runtime"))?;
    let days = bounded_integer(parsed.value("days"), "--days", DEFAULT_DAYS, MAX_LIMIT)?;
    let mut conditions = vec![format!(
        "ts >= current_timestamp - CAST({} AS INTERVAL)",
        quote_sql(format!("{days} days"))
    )];
    if let Some(runtime) = runtime {
        conditions.push(format!("runtime = {}", quote_sql(runtime)));
    }
    run_duck_query(
        &format!(
            "SELECT runtime, count(*) AS events, count(DISTINCT session_id) AS sessions, \
             count(*) FILTER (WHERE event_type = 'user') AS user_msgs, \
             count(*) FILTER (WHERE event_type = 'assistant') AS assistant_msgs, \
             count(*) FILTER (WHERE event_type = 'tool_call') AS tool_calls, \
             sum(tokens_in) AS tokens_in, sum(tokens_out) AS tokens_out, min(ts) AS first_ts, \
             max(ts) AS last_ts FROM events WHERE {} GROUP BY runtime ORDER BY events DESC",
            conditions.join(" AND ")
        ),
        parsed.flag("json"),
        false,
    )
}

pub fn hooks(rest: &[String]) -> Result<i32> {
    let parsed = parse_options("hooks", rest, &["decision", "tool", "limit"], &["json"])?;
    require_flags_only("hooks", &parsed)?;
    let limit = bounded_integer(parsed.value("limit"), "--limit", DEFAULT_LIMIT, MAX_LIMIT)?;
    let mut conditions = Vec::new();
    if let Some(decision) = parsed.value("decision") {
        conditions.push(format!("decision = {}", quote_sql(decision)));
    }
    // The hook that made the decision is what an operator calls the tool here.
    if let Some(tool) = parsed.value("tool") {
        conditions.push(format!("hook_id = {}", quote_sql(tool)));
    }
    run_duck_query(
        &format!(
            "SELECT ts, session_id, project, hook_id, decision, hook_event, infra, reason \
             FROM hook_decisions{} ORDER BY ts DESC LIMIT {limit}",
            where_clause(&conditions)
        ),
        parsed.flag("json"),
        false,
    )
}

pub fn signals(rest: &[String]) -> Result<i32> {
    let parsed = parse_options("signals", rest, &["report", "limit"], &["json"])?;
    require_flags_only("signals", &parsed)?;
    let view = match parsed.value("report").unwrap_or("freshness") {
        "frustration" => "oko_frustration",
        "overlap" => "hook_frustration_overlap",
        "daily" => "hook_frustration_daily",
        "freshness" => "oko_lake_freshness",
        _ => {
            return Err(Error(
                "--report must be frustration, overlap, daily, or freshness".into(),
            ))
        }
    };
    let limit = bounded_integer(parsed.value("limit"), "--limit", DEFAULT_LIMIT, MAX_LIMIT)?;
    // Signal views cross Oko with the Lake, so they are loaded on demand
    // rather than on every read command.
    run_duck_query(
        &format!("SELECT * FROM {view} LIMIT {limit}"),
        parsed.flag("json"),
        true,
    )
}

pub fn query(rest: &[String]) -> Result<i32> {
    let parsed = parse_options("query", rest, &[], &["json"])?;
    let sql = parsed.positionals.join(" ").trim().to_string();
    if sql.is_empty() {
        return Err(Error(
            "usage: transcript-lake query [--json] \"<sql>\"".into(),
        ));
    }
    run_duck_query(&sql, parsed.flag("json"), false)
}
