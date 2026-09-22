//! Derived artifacts and the Oko handoff: Parquet mirrors, the canonical
//! per-session export, the Oko reindex, and removal of rebuildable data.
//! NDJSON partitions are authoritative and none of these commands delete them.
use std::fs;
use std::path::PathBuf;

use serde_json::{json, Map, Value};

use crate::args::{parse_options, require_flags_only, require_no_args, require_runtime};
use crate::cursors::open_writer_lease;
use crate::oko_export;
use crate::paths::{lake_paths, partition_report, path_size, resolve_data_dir};
use crate::util::{find_on_path, quote_sql, run_binary, write_json, Error, Result};

/// Rebuild the per-runtime Parquet mirror of each NDJSON partition set. The
/// mirror is additive: DuckDB rewrites `events.parquet` and the NDJSON stays.
pub fn compact(rest: &[String]) -> Result<i32> {
    let parsed = parse_options("compact", rest, &["source"], &["json"])?;
    require_flags_only("compact", &parsed)?;
    let source = require_runtime(parsed.value("source"))?;
    let data_dir = resolve_data_dir(None);
    let mut rows = partition_report(&data_dir);
    if let Some(source) = &source {
        rows.retain(|row| &row.runtime == source);
    }
    if rows.is_empty() {
        return Err(Error(format!(
            "no matching partitions under {} (start the stream first)",
            data_dir.join("events").display()
        )));
    }
    let _lease = open_writer_lease(&data_dir)?;
    let mut report: Vec<Value> = Vec::new();
    let mut exit_code = 0;
    for row in &rows {
        let runtime_name = format!("runtime={}", row.runtime);
        let source_glob = data_dir
            .join("events")
            .join(&runtime_name)
            .join("*")
            .join("*.ndjson");
        let out_dir = data_dir.join("parquet").join(&runtime_name);
        fs::create_dir_all(&out_dir)?;
        let out_file = out_dir.join("events.parquet");
        // The mirror is a raw NDJSON-to-Parquet COPY and deliberately does not
        // load the canonical views: they would add nothing to this statement,
        // and prepending them displaces the failing line so DuckDB drops the
        // `LINE 1:` excerpt operators rely on when a runtime glob is empty.
        let script = format!(
            "COPY (SELECT * FROM read_ndjson_auto({}, filename=true)) TO {} (FORMAT PARQUET);",
            quote_sql(source_glob.to_string_lossy()),
            quote_sql(out_file.to_string_lossy())
        );
        let status = run_binary("duckdb", &["-c", &script])?;
        if status != 0 {
            eprintln!("compact: duckdb failed for {runtime_name}");
            exit_code = status;
            report.push(json!({
                "runtime": row.runtime,
                "sourceBytes": row.bytes,
                "output": out_file.to_string_lossy(),
                "status": status,
            }));
            continue;
        }
        let parquet_bytes = fs::metadata(&out_file)?.len();
        report.push(json!({
            "runtime": row.runtime,
            "sourceBytes": row.bytes,
            "parquetBytes": parquet_bytes,
            "output": out_file.to_string_lossy(),
            "status": status,
        }));
        if !parsed.flag("json") {
            println!(
                "{}: ndjson {} bytes -> parquet {} bytes ({})",
                row.runtime,
                row.bytes,
                parquet_bytes,
                out_file.display()
            );
        }
    }
    if parsed.flag("json") {
        write_json(&report)?;
    }
    Ok(exit_code)
}

/// Reconstruct the canonical per-session projection Oko imports, and
/// optionally ask Oko to reindex it. The live stream owns incremental updates.
pub fn rebuild_oko(rest: &[String]) -> Result<i32> {
    let parsed = parse_options("rebuild-oko", rest, &[], &["reindex"])?;
    require_flags_only("rebuild-oko", &parsed)?;
    let reindex = parsed.flag("reindex");
    let summary = oko_export::export_oko_with_reindex(true, reindex, &resolve_data_dir(None))?;
    write_json(&summary)?;
    let reindexed = summary.get("reindex").is_some_and(|report| {
        report.get("ran").and_then(Value::as_bool).unwrap_or(false)
            && report.get("status").and_then(Value::as_i64) == Some(0)
    });
    if reindex && !reindexed {
        return Ok(1);
    }
    Ok(0)
}

/// Hand the current export to Oko through its own CLI.
pub fn oko_refresh(rest: &[String]) -> Result<i32> {
    require_no_args("oko-refresh", rest)?;
    let binary = std::env::var_os("OKO_CLI")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .or_else(|| find_on_path("oko-cli"));
    let Some(binary) = binary else {
        eprintln!(
            "oko-cli is not on PATH; install Oko or set OKO_CLI, then run: oko-cli transcripts reindex"
        );
        return Ok(1);
    };
    run_binary(&binary.to_string_lossy(), &["transcripts", "reindex"])
}

/// Preview or remove derived data. Only rebuildable artifacts are ever
/// selected: the Parquet mirrors, the Oko export, and its staging scratch.
pub fn clean(rest: &[String]) -> Result<i32> {
    let parsed = parse_options("clean", rest, &["target"], &["apply", "json"])?;
    require_flags_only("clean", &parsed)?;
    let target = parsed.value("target").unwrap_or("all");
    if !["parquet", "oko", "all"].contains(&target) {
        return Err(Error("--target must be parquet, oko, or all".to_string()));
    }
    let paths = lake_paths();
    let mut selected: Vec<(&str, PathBuf)> = Vec::new();
    if target == "parquet" || target == "all" {
        selected.push(("parquet", paths.parquet));
    }
    if target == "oko" || target == "all" {
        selected.push(("oko", paths.oko_export));
        selected.push(("oko-staging", paths.oko_staging));
    }
    let apply = parsed.flag("apply");
    // The lease is taken only for a removal that has something to remove, so a
    // preview never blocks on an active stream.
    let _lease = if apply && selected.iter().any(|(_, path)| path.exists()) {
        Some(open_writer_lease(&paths.data_dir)?)
    } else {
        None
    };
    let mut report: Vec<Value> = Vec::new();
    for (name, path) in &selected {
        let mut entry = Map::new();
        entry.insert("target".to_string(), json!(name));
        entry.insert("path".to_string(), json!(path.to_string_lossy()));
        entry.insert("exists".to_string(), json!(path.exists()));
        entry.insert("bytes".to_string(), json!(path_size(path)?));
        entry.insert("applied".to_string(), json!(apply));
        report.push(Value::Object(entry));
    }
    if apply {
        for (_, path) in &selected {
            match fs::remove_dir_all(path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotADirectory => {
                    fs::remove_file(path)?;
                }
                Err(error) => return Err(error.into()),
            }
        }
    }
    if parsed.flag("json") {
        write_json(&report)?;
    } else {
        for entry in &report {
            println!(
                "{} {}: {} ({} bytes)",
                if apply { "removed" } else { "would remove" },
                entry["target"].as_str().unwrap_or_default(),
                entry["path"].as_str().unwrap_or_default(),
                entry["bytes"]
            );
        }
        if !apply {
            println!("preview only; add --apply to remove derived data");
        }
    }
    Ok(0)
}
