//! One complete canonical read while Lake's partition writer cannot append.
//! A malformed line is a refusal in the canonical view, not a missing event.

use crate::cursors::open_writer_lease;
use crate::lake::duck::views_script;
use crate::lake::paths::resolve_data_dir;
use crate::util::{run_binary, Error, Result};

/// Run one DuckDB statement against a stable Lake root. An active writer is
/// named by the lease refusal instead of returning a partial source.
pub fn run_query(sql: &str, json: bool) -> Result<i32> {
    let data_dir = resolve_data_dir(None);
    let _lease = open_writer_lease(&data_dir)
        .map_err(|error| Error(format!("cannot read a consistent Transcript Lake snapshot: {error}")))?;
    let script = views_script(sql, false)?;
    let mut args = Vec::new();
    if json {
        args.push("-json".to_string());
    }
    args.push("-c".to_string());
    args.push(script);
    run_binary("duckdb", &args)
}
