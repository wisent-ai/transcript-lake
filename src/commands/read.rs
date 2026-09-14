//! Read-only analytics over the canonical DuckDB views. Every command here
//! builds one statement and hands it to `crate::duck`, which loads the views
//! first; DuckDB renders its own table or `-json` output, so the exit status
//! of the child is the exit status of the command. `show` is the exception: it
//! reads rows back as JSON because it reconstructs a whole conversation and
//! reports how much of it the limit cut. It lives in `read/show.rs`, which is
//! where its `--out` destination and refusals are documented.

mod show;

pub use show::show;

use serde_json::Value;

use crate::args::{
    bounded_integer, parse_options, require_flags_only, require_runtime, DEFAULT_DAYS,
    DEFAULT_LIMIT, MAX_LIMIT,
};
use crate::duck::run_duck_query;
use crate::util::{quote_sql, Error, Result};

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
