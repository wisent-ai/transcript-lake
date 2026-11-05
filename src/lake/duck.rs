//! DuckDB bridge. The canonical views are compiled into the binary and loaded
//! ahead of every statement, so a moved or partially copied installation can
//! never present a Lake without its views. DuckDB itself stays an external
//! optional dependency: absence is an actionable error, never a silent
//! fallback to another engine.
use std::path::PathBuf;
use std::process::Command;

use serde_json::Value;

use crate::paths::resolve_data_dir;
use crate::util::{quote_sql, Error, Result};

pub(crate) mod memory;

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
    if !crate::lake::paths::present(&path)? {
        return Err(Error(format!(
            "missing {} (TRANSCRIPT_LAKE_SQL is set but incomplete)",
            path.display()
        )));
    }
    Ok(std::fs::read_to_string(&path)?)
}

/// The full script for one query: the memory limit measured now and the
/// spill directory under the Lake root, the data-dir variable, the canonical
/// views the statement needs (every one when the signal views are included,
/// since they are written over them), optionally the signal views, then the
/// caller's SQL; with the Lake root's statement lock, which the caller holds
/// until DuckDB ends (`memory::settings`).
pub(crate) fn views_script(sql: &str, include_signals: bool) -> Result<(String, memory::Slot)> {
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
    let (settings, slot) = memory::settings(&data_dir)?;
    let script = format!(
        "{settings}SET VARIABLE lake_data = {};\n{setup}\n{sql}",
        quote_sql(data_dir.to_string_lossy())
    );
    Ok((script, slot))
}

/// `duckdb` with `script` on its standard input instead of in its arguments:
/// a statement that names many sessions does not fit the argument list the
/// system allows (`Argument list too long`). `-bail` keeps the meaning of
/// `-c`: the first failing statement ends the script with status 1 and no
/// later statement runs.
pub(crate) fn duckdb(json: bool) -> Command {
    let mut command = Command::new("duckdb");
    if json {
        command.arg("-json");
    }
    command.arg("-bail").stdin(std::process::Stdio::piped());
    command
}

/// Start `command` (from [`duckdb`]) and write `script` to its standard
/// input, then close it so DuckDB runs the script and ends.
pub(crate) fn fed(command: &mut Command, script: &str) -> Result<std::process::Child> {
    let mut child = command.spawn().map_err(|error| Error(format!("duckdb failed to start: {error}")))?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| Error("duckdb started with no standard input to read its script from".into()))?;
    std::io::Write::write_all(&mut stdin, script.as_bytes())
        .map_err(|error| Error(format!("the script could not be written to duckdb's standard input: {error}")))?;
    drop(stdin);
    Ok(child)
}

/// Run a statement with inherited output, so DuckDB renders its own table or
/// JSON output. Returns the exit status the CLI should adopt.
pub fn run_duck_query(sql: &str, json: bool, include_signals: bool) -> Result<i32> {
    let (script, _slot) = views_script(sql, include_signals)?;
    run_fed(&script, json)
}

/// [`run_duck_query`] for a whole script already built.
pub(crate) fn run_fed(script: &str, json: bool) -> Result<i32> {
    let child = fed(&mut duckdb(json), script)?;
    let status = stado_wait::child_output(child, "duckdb statement")
        .map_err(|error| Error(format!("duckdb failed to finish: {error}")))?
        .status;
    status.code().ok_or_else(|| Error(format!("duckdb terminated: {status}")))
}

/// Run a statement and parse its JSON rows for programmatic use.
pub fn query_duck_json(sql: &str) -> Result<Vec<Value>> {
    let (script, _slot) = views_script(sql, false)?;
    let mut command = duckdb(true);
    command.stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped());
    let child = fed(&mut command, &script)?;
    let output = stado_wait::child_output(child, "duckdb JSON statement")
        .map_err(|error| Error(format!("duckdb failed to finish: {error}")))?;
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
