//! Screens as they are shown, plus the terminal verdict. Human output is
//! printed as the walk happens; `--json` collects the same walk into one
//! object, because a machine reader wants one document, not a transcript.

use serde_json::{json, Map, Value};

use crate::util::{Error, Result};

use super::journey::*;
use super::*;

/// Screens as they are shown, plus the terminal verdict. Human output is
/// printed as the walk happens; `--json` collects the same walk into one
/// object, because a machine reader wants one document, not a transcript.
pub(super) struct Report {
    journey_version: String,
    reset: bool,
    json: bool,
    steps: Vec<Value>,
    status: &'static str,
    current_screen_id: String,
    next: String,
}

impl Report {
    pub(super) fn new(definition: &Value, reset: bool, json: bool) -> Result<Self> {
        Ok(Self {
            journey_version: required(
                definition,
                "journey_version",
                "the canonical onboarding journey",
            )?,
            reset,
            json,
            steps: Vec::new(),
            status: "in_progress",
            current_screen_id: String::new(),
            next: String::new(),
        })
    }

    /// Show `screen`: its title and body, which [`canonical_definition`]
    /// requires of every screen, so a screen without them is refused here
    /// rather than shown under an invented title or as an empty page.
    pub(super) fn render(&mut self, screen: &Value) -> Result<()> {
        let id = screen.get("screen_id").and_then(Value::as_str);
        let Some(presentation) = screen.get("presentation") else {
            return Err(Error(format!(
                "onboarding screen {id:?} has no presentation"
            )));
        };
        let shown = format!("the presentation of onboarding screen {id:?}");
        let title = required(presentation, "title", &shown)?;
        let body = required(presentation, "body", &shown)?;
        let command = presentation.get("command").and_then(Value::as_str);
        if self.json {
            self.steps.push(json!({
                "screen_id": screen.get("screen_id"),
                "screen_kind": screen.get("screen_kind"),
                "title": title,
                "body": body,
                "command": command,
                "actions": screen.get("actions"),
            }));
            return Ok(());
        }
        println!("\n== {title} ==\n{body}");
        if let Some(command) = command {
            println!("command: {command}");
        }
        Ok(())
    }

    /// A line about what this machine actually holds, printed under the screen
    /// it qualifies. In `--json` it belongs to the step it followed.
    pub(super) fn note(&mut self, text: &str) {
        if !self.json {
            println!("{text}");
            return;
        }
        if let Some(step) = self.steps.last_mut() {
            match step.get_mut("notes").and_then(Value::as_array_mut) {
                Some(notes) => notes.push(Value::String(text.to_string())),
                None => step["notes"] = Value::Array(vec![Value::String(text.to_string())]),
            }
        }
    }

    /// The rows the first query returned: the first result of the product.
    pub(super) fn rows(&mut self, rows: &[Value]) {
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

    /// End the walk at the screen `state` stands on. A state without one is
    /// refused: reported as an empty screen id, a reader could not tell where
    /// the next run resumes.
    pub(super) fn finish(&mut self, status: &'static str, state: &Value, next: &str) -> Result<()> {
        self.status = status;
        self.current_screen_id = required(state, "current_screen_id", "the onboarding state")?;
        self.next = next.to_string();
        Ok(())
    }

    pub(super) fn emit(self) -> Result<i32> {
        if !self.json {
            println!("\nstatus: {}", self.status);
            println!("next: {}", self.next);
            return Ok(libc::EXIT_SUCCESS);
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
        Ok(libc::EXIT_SUCCESS)
    }
}

/// The text `key` of `value`, refused naming the key and `what` holds it.
fn required(value: &Value, key: &str, what: &str) -> Result<String> {
    string_field(value, key).ok_or_else(|| Error(format!("{what} has no {key} text")))
}
