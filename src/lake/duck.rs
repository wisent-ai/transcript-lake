//! DuckDB bridge. The canonical views are compiled into the binary and loaded
//! ahead of every statement, so a moved or partially copied installation can
//! never present a Lake without its views. DuckDB itself stays an external
//! optional dependency: absence is an actionable error, never a silent
//! fallback to another engine.
use std::path::PathBuf;
use std::process::Command;

use serde_json::Value;

use crate::paths::resolve_data_dir;
use crate::util::{quote_sql, run_binary, Error, Result};

/// The canonical views, embedded at build time from `sql/`.
const VIEWS_SQL: &str = include_str!("../../sql/views.sql");
const SIGNALS_SQL: &str = include_str!("../../sql/signals.sql");

/// Read one embedded script, or its override from `TRANSCRIPT_LAKE_SQL`.
///
/// The override exists so an operator can iterate on the view definitions
/// against an installed binary; a directory that is set but incomplete is an
/// error rather than a silent fall back to the embedded copy, because a
/// half-overridden view set would report evidence nobody can reproduce.
fn script(name: &str, embedded: &'static str) -> Result<String> {
    let Some(dir) = std::env::var_os("TRANSCRIPT_LAKE_SQL").filter(|dir| !dir.is_empty()) else {
        return Ok(embedded.to_string());
    };
    let path = PathBuf::from(dir).join(name);
    if !path.exists() {
        return Err(Error(format!(
            "missing {} (TRANSCRIPT_LAKE_SQL is set but incomplete)",
            path.display()
        )));
    }
    Ok(std::fs::read_to_string(&path)?)
}

/// The full script for one query: data-dir variable, the canonical views the
/// statement needs (every one when the signal views are included, since they
/// are written over them), optionally the signal views, then the caller's SQL.
pub fn views_script(sql: &str, include_signals: bool) -> Result<String> {
    let mut setup = super::views::needed(&script("views.sql", VIEWS_SQL)?, sql, include_signals)?;
    if include_signals {
        // ATTACH takes a literal path and DuckDB does not expand a tilde, so
        // the placeholder in signals.sql is resolved here, against the same
        // location the Oko export probes for freshness.
        let oko_db = crate::oko_export::oko_index_path();
        setup.push('\n');
        setup.push_str(
            &script("signals.sql", SIGNALS_SQL)?
                .replace("'__OKO_DB__'", &quote_sql(oko_db.to_string_lossy())),
        );
    }
    let data_dir = resolve_data_dir(None);
    Ok(format!(
        "SET VARIABLE lake_data = {};\n{setup}\n{sql}",
        quote_sql(data_dir.to_string_lossy())
    ))
}

/// Run a statement with inherited stdio, so DuckDB renders its own table or
/// JSON output. Returns the exit status the CLI should adopt.
pub fn run_duck_query(sql: &str, json: bool, include_signals: bool) -> Result<i32> {
    let script = views_script(sql, include_signals)?;
    let mut args: Vec<String> = Vec::new();
    if json {
        args.push("-json".to_string());
    }
    args.push("-c".to_string());
    args.push(script);
    run_binary("duckdb", &args)
}

/// Run a statement and parse its JSON rows for programmatic use.
pub fn query_duck_json(sql: &str) -> Result<Vec<Value>> {
    let script = views_script(sql, false)?;
    let mut command = Command::new("duckdb");
    command.args(["-json", "-c", &script]);
    let output = stado_wait::output(&mut command)
        .map_err(|error| Error(format!("duckdb failed to start: {error}")))?;
    if output.status.code().is_none() {
        return Err(Error("duckdb terminated by signal".into()));
    }
    if !output.status.success() {
        return Err(Error(format!(
            "duckdb exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let text = text.trim();
    if text.is_empty() {
        return Ok(Vec::new());
    }
    match serde_json::from_str::<Value>(text)? {
        Value::Array(rows) => Ok(rows),
        other => Ok(vec![other]),
    }
}
