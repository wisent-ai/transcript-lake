//! First-use walkthrough. The journey Echo publishes for this product is the
//! one shipped in `assets/onboarding_first_use.json`, compiled
//! into the binary; this command walks that definition rather than a second
//! copy of the same words, so the screens an operator reads here are the
//! screens the control plane holds.
//!
//! Progress is recorded per machine outside the Lake: it is operator state,
//! not transcript evidence, so `--data-dir`, `clean` and `rebuild` never move
//! or remove it. `--reset` discards the recorded attempt and replays the
//! journey from its entry screen in the same invocation.
use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::args::{parse_options, require_flags_only};
use crate::duck::query_duck_json;
use crate::paths::resolve_data_dir;
use crate::util::{home_dir, machine_name, write_json, Error, Result};

const PRODUCT_ID: &str = "transcript-lake";
const JOURNEY_ID: &str = "first-use";
const STATE_SCHEMA: &str = "transcript-lake.onboarding-state.v1";
const SOURCE_FACT: &str = "transcript_source_adopted";
const FIRST_SUCCESS_FACT: &str = "lake_query_rows_returned";

/// The published definition, embedded at build time from the file Echo's
/// publisher discovers at `origin/main`.
const DEFINITION: &str = include_str!("../../../assets/onboarding_first_use.json");

/// The first question the journey asks of the archive: one row per runtime
/// that has ever been captured, which is exactly the evidence a closed
/// terminal cannot produce.
const FIRST_QUERY: &str = "SELECT runtime, count(*) AS events, \
count(DISTINCT session_id) AS sessions FROM events GROUP BY runtime ORDER BY events DESC";

pub fn onboarding(rest: &[String]) -> Result<i32> {
    let parsed = parse_options(
        "onboarding",
        rest,
        &["source", "root"],
        &["reset", "yes", "json", "skip-source"],
    )?;
    require_flags_only("onboarding", &parsed)?;
    let source = parsed.value("source").map(str::to_string);
    let root = parsed.value("root").map(str::to_string);
    if source.is_some() != root.is_some() {
        return Err(Error(
            "onboarding requires --source and --root together".into(),
        ));
    }
    if parsed.flag("skip-source") && source.is_some() {
        return Err(Error(
            "use either --source/--root or --skip-source".into(),
        ));
    }
    let json_output = parsed.flag("json");
    // Machine output has no reader to press Enter, so it never prompts.
    let unattended = parsed.flag("yes") || json_output;
    let definition = canonical_definition()?;
    let revision = format!("transcript-lake-{}", crate::VERSION);
    let mut state = load_or_start_state(&definition, &revision, parsed.flag("reset"))?;
    let mut report = Report::new(&definition, parsed.flag("reset"), json_output);

    if state.get("status").and_then(Value::as_str) == Some("completed") {
        report.finish(
            "completed",
            &state,
            "The Lake already answered a query on this machine. Continue with: transcript-lake help query",
        );
        return report.emit();
    }

    loop {
        let screen_id = string_field(&state, "current_screen_id")
            .ok_or_else(|| Error("onboarding state has no current screen".into()))?;
        let screen = screen_by_id(&definition, &screen_id)?.clone();
        report.render(&screen);

        match screen.get("screen_kind").and_then(Value::as_str) {
            Some("first_action") => {
                let data_dir = resolve_data_dir(None);
                let mut selected = crate::sources::selected_source(&data_dir)?;
                if parsed.flag("skip-source") {
                    report.note(
                        "Source adoption was skipped. Existing Lake state is unchanged, and no new first-success fact was recorded.",
                    );
                    report.finish(
                        "skipped_source",
                        &state,
                        "Run transcript-lake sources, then transcript-lake adopt --source <runtime> --root <path> whenever you want to seed or change this Lake.",
                    );
                    return report.emit();
                }
                if let (Some(runtime), Some(root)) = (source.as_deref(), root.as_deref()) {
                    let adoption = crate::commands::adopt::adopt_source(
                        runtime,
                        std::path::Path::new(root),
                        &data_dir,
                    )?;
                    report.note(&format!(
                        "Selected {}: {} file(s) imported, {} unchanged, {} canonical event(s) imported.",
                        adoption.get("sourceId").and_then(Value::as_str).unwrap_or_default(),
                        adoption.get("imported").and_then(Value::as_u64).unwrap_or(0),
                        adoption.get("unchanged").and_then(Value::as_u64).unwrap_or(0),
                        adoption.get("eventsImported").and_then(Value::as_u64).unwrap_or(0),
                    ));
                    selected = crate::sources::selected_source(&data_dir)?;
                } else if selected.is_none() {
                    let sources = crate::commands::inspect::source_report()?;
                    let candidates: Vec<String> = sources
                        .iter()
                        .filter(|candidate| candidate.available && candidate.mode == "transcripts")
                        .flat_map(|candidate| {
                            candidate.roots.iter().map(move |root| {
                                format!("{}: {} ({} files)", candidate.runtime, root, candidate.files)
                            })
                        })
                        .collect();
                    if candidates.is_empty() {
                        report.note("No supported transcript roots were discovered on this machine.");
                        report.finish(
                            "awaiting_source",
                            &state,
                            "Create work with Claude Code, Codex, omp, Factory Droid, or Kimi, then run: transcript-lake onboarding",
                        );
                    } else {
                        for candidate in &candidates {
                            report.note(&format!("Discovered {candidate}"));
                        }
                        report.finish(
                            "awaiting_source_selection",
                            &state,
                            "Choose one discovered root, then run: transcript-lake onboarding --source <runtime> --root <path>",
                        );
                    }
                    return report.emit();
                }
                let selected = selected.ok_or_else(|| {
                    Error("source adoption returned without a persisted selection".into())
                })?;
                report.note(&format!(
                    "This Lake follows {} at {} as {}.",
                    selected.runtime,
                    selected.root.display(),
                    selected.id
                ));
                let evidence = fact(SOURCE_FACT);
                advance(&definition, &screen, &mut state, &evidence, &revision)?.ok_or_else(
                    || Error("an adopted source does not satisfy the published journey".into()),
                )?;
            }
            Some("first_success") => {
                report.note(&format!("Running: transcript-lake query \"{FIRST_QUERY}\""));
                let rows = match query_duck_json(FIRST_QUERY) {
                    Ok(rows) => rows,
                    Err(error) => {
                        report.note(&format!("The query could not run: {error}"));
                        report.finish(
                            "awaiting_duckdb",
                            &state,
                            "Install DuckDB 1.5.x on PATH, then run: transcript-lake onboarding",
                        );
                        return report.emit();
                    }
                };
                if rows.is_empty() {
                    report.finish(
                        "awaiting_events",
                        &state,
                        "The query returned no rows yet; leave the stream running, then run: transcript-lake onboarding",
                    );
                    return report.emit();
                }
                report.rows(&rows);
                wait_for_enter(unattended, "Press Enter to finish onboarding. ")?;
                let evidence = fact(FIRST_SUCCESS_FACT);
                if !complete(&screen, &mut state, &evidence, &revision)? {
                    return Err(Error(
                        "published first-success evidence was not satisfied".into(),
                    ));
                }
                report.finish(
                    "completed",
                    &state,
                    "Ask your own question with: transcript-lake query \"<sql>\"",
                );
                return report.emit();
            }
            Some(_) => {
                wait_for_enter(unattended, "Press Enter to continue. ")?;
                advance(&definition, &screen, &mut state, &Map::new(), &revision)?.ok_or_else(
                    || Error("published journey has no eligible next screen".into()),
                )?;
            }
            None => return Err(Error("published onboarding screen has no kind".into())),
        }
    }
}

/// One fact, asserted true: the only evidence shape the shipped journey uses.
pub(super) fn fact(name: &str) -> Map<String, Value> {
    Map::from_iter([(name.to_string(), Value::Bool(true))])
}

mod journey;
mod report;
mod state;

use journey::*;
use report::Report;
use state::*;
