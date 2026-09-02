//! First-use walkthrough. The journey Echo publishes for this product is the
//! one shipped in `onboarding_first_use.json` at the repository root, compiled
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
use crate::paths::{partition_report, resolve_data_dir};
use crate::util::{home_dir, machine_name, write_json, Error, Result};

const PRODUCT_ID: &str = "transcript-lake";
const JOURNEY_ID: &str = "first-use";
const STATE_SCHEMA: &str = "transcript-lake.onboarding-state.v1";
const PARTITIONS_FACT: &str = "lake_partitions_observed";
const FIRST_SUCCESS_FACT: &str = "lake_query_rows_returned";

/// The published definition, embedded at build time from the file Echo's
/// publisher discovers at `origin/main`.
const DEFINITION: &str = include_str!("../../onboarding_first_use.json");

/// The first question the journey asks of the archive: one row per runtime
/// that has ever been captured, which is exactly the evidence a closed
/// terminal cannot produce.
const FIRST_QUERY: &str = "SELECT runtime, count(*) AS events, \
count(DISTINCT session_id) AS sessions FROM events GROUP BY runtime ORDER BY events DESC";

pub fn onboarding(rest: &[String]) -> Result<i32> {
    let parsed = parse_options("onboarding", rest, &[], &["reset", "yes", "json"])?;
    require_flags_only("onboarding", &parsed)?;
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
                let parts: u64 = partition_report(&resolve_data_dir(None))
                    .iter()
                    .map(|row| row.parts)
                    .sum();
                if parts == 0 {
                    report.note("This Lake holds no partitions yet, so there is nothing to query.");
                    report.finish(
                        "awaiting_stream",
                        &state,
                        "Start the stream, leave it running, then run: transcript-lake onboarding",
                    );
                    return report.emit();
                }
                report.note(&format!(
                    "This Lake holds {parts} masked partition files of your own transcripts."
                ));
                let evidence = fact(PARTITIONS_FACT);
                advance(&definition, &screen, &mut state, &evidence, &revision)?.ok_or_else(
                    || Error("observed partitions do not satisfy the published journey".into()),
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
fn fact(name: &str) -> Map<String, Value> {
    Map::from_iter([(name.to_string(), Value::Bool(true))])
}

/// Screens as they are shown, plus the terminal verdict. Human output is
/// printed as the walk happens; `--json` collects the same walk into one
/// object, because a machine reader wants one document, not a transcript.
struct Report {
    journey_version: String,
    reset: bool,
    json: bool,
    steps: Vec<Value>,
    status: &'static str,
    current_screen_id: String,
    next: String,
}

impl Report {
    fn new(definition: &Value, reset: bool, json: bool) -> Self {
        Self {
            journey_version: string_field(definition, "journey_version").unwrap_or_default(),
            reset,
            json,
            steps: Vec::new(),
            status: "in_progress",
            current_screen_id: String::new(),
            next: String::new(),
        }
    }

    fn render(&mut self, screen: &Value) {
        let presentation = screen.get("presentation");
        let title = presentation
            .and_then(|value| value.get("title"))
            .and_then(Value::as_str)
            .unwrap_or("Transcript Lake onboarding");
        let body = presentation
            .and_then(|value| value.get("body"))
            .and_then(Value::as_str)
            .unwrap_or_default();
        let command = presentation
            .and_then(|value| value.get("command"))
            .and_then(Value::as_str);
        if self.json {
            self.steps.push(json!({
                "screen_id": screen.get("screen_id"),
                "screen_kind": screen.get("screen_kind"),
                "title": title,
                "body": body,
                "command": command,
                "actions": screen.get("actions"),
            }));
            return;
        }
        println!("\n== {title} ==\n{body}");
        if let Some(command) = command {
            println!("command: {command}");
        }
    }

    /// A line about what this machine actually holds, printed under the screen
    /// it qualifies. In `--json` it belongs to the step it followed.
    fn note(&mut self, text: &str) {
        if !self.json {
            println!("{text}");
            return;
        }
        if let Some(step) = self.steps.last_mut() {
            let notes = step
                .get_mut("notes")
                .and_then(Value::as_array_mut)
                .map(std::mem::take);
            let mut notes = notes.unwrap_or_default();
            notes.push(Value::String(text.to_string()));
            step["notes"] = Value::Array(notes);
        }
    }

    /// The rows the first query returned: the first result of the product.
    fn rows(&mut self, rows: &[Value]) {
        if self.json {
            if let Some(step) = self.steps.last_mut() {
                step["rows"] = Value::Array(rows.to_vec());
            }
            return;
        }
        println!("{} row(s):", rows.len());
        for row in rows {
            println!("  {row}");
        }
    }

    fn finish(&mut self, status: &'static str, state: &Value, next: &str) {
        self.status = status;
        self.current_screen_id = string_field(state, "current_screen_id").unwrap_or_default();
        self.next = next.to_string();
    }

    fn emit(self) -> Result<i32> {
        if !self.json {
            println!("\nstatus: {}", self.status);
            println!("next: {}", self.next);
            return Ok(0);
        }
        write_json(&json!({
            "product_id": PRODUCT_ID,
            "journey_id": JOURNEY_ID,
            "journey_version": self.journey_version,
            "status": self.status,
            "current_screen_id": self.current_screen_id,
            "first_success_fact": FIRST_SUCCESS_FACT,
            "reset": self.reset,
            "steps": self.steps,
            "next": self.next,
        }))?;
        Ok(0)
    }
}

/// The embedded definition, checked for the identity and the graph this
/// command relies on before a single screen is shown.
fn canonical_definition() -> Result<Value> {
    let definition: Value = serde_json::from_str(DEFINITION)?;
    if definition.get("schema_version").and_then(Value::as_u64) != Some(1)
        || definition.get("product_id").and_then(Value::as_str) != Some(PRODUCT_ID)
        || definition.get("journey_id").and_then(Value::as_str) != Some(JOURNEY_ID)
        || definition.get("first_success_fact").and_then(Value::as_str) != Some(FIRST_SUCCESS_FACT)
    {
        return Err(Error("canonical onboarding journey identity mismatch".into()));
    }
    let entry = string_field(&definition, "entry_screen_id")
        .ok_or_else(|| Error("canonical onboarding journey has no entry screen".into()))?;
    let screens = definition
        .get("screens")
        .and_then(Value::as_array)
        .ok_or_else(|| Error("canonical onboarding journey has no screens".into()))?;
    let mut ids: Vec<&str> = Vec::with_capacity(screens.len());
    for screen in screens {
        let id = screen
            .get("screen_id")
            .and_then(Value::as_str)
            .ok_or_else(|| Error("canonical onboarding screen has no id".into()))?;
        if ids.contains(&id) {
            return Err(Error(format!(
                "duplicate canonical onboarding screen id: {id}"
            )));
        }
        if screen.get("screen_kind").and_then(Value::as_str).is_none()
            || screen.get("presentation").and_then(Value::as_object).is_none()
        {
            return Err(Error(format!(
                "canonical onboarding screen is incomplete: {id}"
            )));
        }
        ids.push(id);
    }
    if !ids.contains(&entry.as_str()) {
        return Err(Error(
            "canonical onboarding entry screen does not exist".into(),
        ));
    }
    for screen in screens {
        for transition in transitions(screen) {
            let next = transition
                .get("next_screen_id")
                .and_then(Value::as_str)
                .ok_or_else(|| Error("canonical onboarding transition has no target".into()))?;
            if !ids.contains(&next) {
                return Err(Error(format!(
                    "canonical onboarding transition target does not exist: {next}"
                )));
            }
        }
    }
    Ok(definition)
}

fn transitions(screen: &Value) -> &[Value] {
    screen
        .get("transitions")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
}

fn screen_by_id<'a>(definition: &'a Value, screen_id: &str) -> Result<&'a Value> {
    definition
        .get("screens")
        .and_then(Value::as_array)
        .and_then(|screens| {
            screens
                .iter()
                .find(|screen| screen.get("screen_id").and_then(Value::as_str) == Some(screen_id))
        })
        .ok_or_else(|| {
            Error(format!(
                "published onboarding screen is unavailable: {screen_id}"
            ))
        })
}

/// The published edge out of this screen: highest priority wins, exactly as
/// the control plane's own selection does.
fn next_screen_id(screen: &Value) -> Option<&str> {
    transitions(screen)
        .iter()
        .max_by_key(|transition| {
            transition
                .get("priority")
                .and_then(Value::as_i64)
                .unwrap_or_default()
        })
        .and_then(|transition| transition.get("next_screen_id").and_then(Value::as_str))
}

/// Whether the evidence this command gathered satisfies what the definition
/// requires of the screen. A screen with no rule is satisfied by arriving.
fn evidence_satisfied(screen: &Value, evidence: &Map<String, Value>) -> Result<bool> {
    let Some(rule) = screen
        .get("completion_evidence")
        .filter(|value| !value.is_null())
    else {
        return Ok(true);
    };
    if rule.get("kind").and_then(Value::as_str) != Some("fact")
        || rule.get("operator").and_then(Value::as_str) != Some("eq")
    {
        return Err(Error("unsupported canonical onboarding evidence rule".into()));
    }
    let name = rule
        .get("fact")
        .and_then(Value::as_str)
        .ok_or_else(|| Error("canonical onboarding evidence rule has no fact".into()))?;
    let expected = rule
        .get("value")
        .ok_or_else(|| Error("canonical onboarding evidence rule has no expected value".into()))?;
    Ok(evidence.get(name) == Some(expected))
}

fn advance(
    definition: &Value,
    screen: &Value,
    state: &mut Value,
    evidence: &Map<String, Value>,
    revision: &str,
) -> Result<Option<String>> {
    if !evidence_satisfied(screen, evidence)? {
        return Ok(None);
    }
    let Some(next) = next_screen_id(screen).map(str::to_string) else {
        return Ok(None);
    };
    screen_by_id(definition, &next)?;
    set(state, "current_screen_id", Value::String(next.clone()))?;
    set(state, "revision", Value::String(revision.to_string()))?;
    save_state(state)?;
    Ok(Some(next))
}

fn complete(
    screen: &Value,
    state: &mut Value,
    evidence: &Map<String, Value>,
    revision: &str,
) -> Result<bool> {
    if !evidence_satisfied(screen, evidence)? {
        return Ok(false);
    }
    set(state, "status", Value::String("completed".into()))?;
    set(state, "revision", Value::String(revision.to_string()))?;
    save_state(state)?;
    Ok(true)
}

fn set(state: &mut Value, key: &str, value: Value) -> Result<()> {
    state
        .as_object_mut()
        .ok_or_else(|| Error("onboarding state is not an object".into()))?
        .insert(key.to_string(), value);
    Ok(())
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

/// Recorded progress for this machine, or a fresh attempt. `reset` discards
/// what was recorded rather than resuming it, which is what replay means here:
/// the walk starts again at the journey's entry screen.
fn load_or_start_state(definition: &Value, revision: &str, reset: bool) -> Result<Value> {
    let path = state_path();
    if !reset && path.exists() {
        let existing: Value = serde_json::from_str(&fs::read_to_string(&path)?)?;
        if existing.get("schema").and_then(Value::as_str) != Some(STATE_SCHEMA)
            || existing.get("product_id").and_then(Value::as_str) != Some(PRODUCT_ID)
            || existing.get("journey_id").and_then(Value::as_str) != Some(JOURNEY_ID)
        {
            return Err(Error(
                "stored onboarding state identity mismatch; use --reset to replace it".into(),
            ));
        }
        // A journey republished with new screens invalidates a screen id that
        // no longer exists; resuming into it would show nothing at all.
        let current = string_field(&existing, "current_screen_id")
            .ok_or_else(|| Error("stored onboarding state has no current screen".into()))?;
        if string_field(&existing, "journey_version") == string_field(definition, "journey_version")
            && screen_by_id(definition, &current).is_ok()
        {
            return Ok(existing);
        }
    }
    let state = json!({
        "schema": STATE_SCHEMA,
        "product_id": PRODUCT_ID,
        "journey_id": JOURNEY_ID,
        "journey_version": definition.get("journey_version"),
        "source_revision": definition.get("source_revision"),
        "subject_hash": subject_hash(),
        "attempt_id": Uuid::new_v4().to_string(),
        "current_screen_id": definition.get("entry_screen_id"),
        "status": "in_progress",
        "revision": revision,
    });
    save_state(&state)?;
    Ok(state)
}

fn save_state(state: &Value) -> Result<()> {
    let path = state_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temporary = PathBuf::from(format!("{}.tmp-{}", path.display(), std::process::id()));
    fs::write(&temporary, format!("{}\n", serde_json::to_string(state)?))?;
    fs::rename(&temporary, &path)?;
    Ok(())
}

/// Progress belongs to this operator on this machine, not to a Lake: a second
/// `--data-dir` is still the same first use.
fn subject_hash() -> String {
    let user = std::env::var("USER").unwrap_or_else(|_| "unknown-user".to_string());
    format!(
        "{:x}",
        Sha256::digest(format!("transcript-lake-onboarding\0{user}\0{}", machine_name()).as_bytes())
    )
}

fn state_path() -> PathBuf {
    std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| home_dir().join(".local").join("state"))
        .join("transcript-lake")
        .join("onboarding.json")
}

fn wait_for_enter(unattended: bool, prompt: &str) -> Result<()> {
    if unattended {
        return Ok(());
    }
    print!("{prompt}");
    io::stdout().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    Ok(())
}
