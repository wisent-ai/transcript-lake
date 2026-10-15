//! What this machine recorded about the walk: where the state file lives,
//! how a resumed walk is told from a replayed one, and the pause between
//! screens that an unattended run does not wait for.

use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::util::{home_dir, machine_name, Error, Result};

use super::*;

/// Recorded progress for this machine, or a fresh attempt. `reset` discards
/// what was recorded rather than resuming it, which is what replay means here:
/// the walk starts again at the journey's entry screen.
pub(super) fn load_or_start_state(
    definition: &Value,
    revision: &str,
    reset: bool,
) -> Result<Value> {
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
        "subject_hash": subject_hash()?,
        "attempt_id": Uuid::new_v4().to_string(),
        "current_screen_id": definition.get("entry_screen_id"),
        "status": "in_progress",
        "revision": revision,
    });
    save_state(&state)?;
    Ok(state)
}

pub(super) fn save_state(state: &Value) -> Result<()> {
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
/// `--data-dir` is still the same first use. The operator is `USER`; without
/// one there is nobody to record progress for, so the walk is refused.
pub(super) fn subject_hash() -> Result<String> {
    let user = std::env::var("USER").map_err(|error| {
        Error(format!(
            "onboarding progress is recorded per operator, and USER names none: {error}"
        ))
    })?;
    Ok(format!(
        "{:x}",
        Sha256::digest(
            format!("transcript-lake-onboarding\0{user}\0{}", machine_name()).as_bytes()
        )
    ))
}

/// `$XDG_STATE_HOME/transcript-lake/onboarding.json`, with the XDG base
/// directory specification's own `~/.local/state` when it names nothing.
pub(super) fn state_path() -> PathBuf {
    let state_home = match std::env::var_os("XDG_STATE_HOME").filter(|path| !path.is_empty()) {
        Some(path) => PathBuf::from(path),
        None => home_dir().join(".local").join("state"),
    };
    state_home.join("transcript-lake").join("onboarding.json")
}

pub(super) fn wait_for_enter(unattended: bool, prompt: &str) -> Result<()> {
    if unattended {
        return Ok(());
    }
    print!("{prompt}");
    io::stdout().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    Ok(())
}
