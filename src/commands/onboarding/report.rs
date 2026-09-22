//! Screens as they are shown, plus the terminal verdict. Human output is
//! printed as the walk happens; `--json` collects the same walk into one
//! object, because a machine reader wants one document, not a transcript.

use serde_json::{json, Map, Value};

use crate::util::Result;

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
    pub(super) fn new(definition: &Value, reset: bool, json: bool) -> Self {
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

    pub(super) fn render(&mut self, screen: &Value) {
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
    pub(super) fn note(&mut self, text: &str) {
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

    pub(super) fn finish(&mut self, status: &'static str, state: &Value, next: &str) {
        self.status = status;
        self.current_screen_id = string_field(state, "current_screen_id").unwrap_or_default();
        self.next = next.to_string();
    }

    pub(super) fn emit(self) -> Result<i32> {
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

