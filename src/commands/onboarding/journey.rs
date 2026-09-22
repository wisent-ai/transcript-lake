//! The journey graph itself: the embedded definition checked before a screen
//! is shown, the transitions between screens, and the evidence a screen
//! requires before the walk may move past it.

use serde_json::{Map, Value};

use crate::util::{Error, Result};

use super::*;

/// The embedded definition, checked for the identity and the graph this
/// command relies on before a single screen is shown.
pub(super) fn canonical_definition() -> Result<Value> {
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

pub(super) fn transitions(screen: &Value) -> &[Value] {
    screen
        .get("transitions")
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default()
}

pub(super) fn screen_by_id<'a>(definition: &'a Value, screen_id: &str) -> Result<&'a Value> {
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
pub(super) fn next_screen_id(screen: &Value) -> Option<&str> {
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
pub(super) fn evidence_satisfied(screen: &Value, evidence: &Map<String, Value>) -> Result<bool> {
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

pub(super) fn advance(
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

pub(super) fn complete(
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

pub(super) fn set(state: &mut Value, key: &str, value: Value) -> Result<()> {
    state
        .as_object_mut()
        .ok_or_else(|| Error("onboarding state is not an object".into()))?
        .insert(key.to_string(), value);
    Ok(())
}

pub(super) fn string_field(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_string)
}

/// Recorded progress for this machine, or a fresh attempt. `reset` discards
