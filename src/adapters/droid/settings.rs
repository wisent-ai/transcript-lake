//! The sidecar a session writes beside its transcript: read once at end, it
//! yields at most one meta event carrying the token totals it recorded.

use serde_json::{Map, Value};

use crate::types::{Parser, ParserCtx, RawEvent};

use crate::adapters::common::{int_field, prune, stored_json};

/// A sidecar is small and is not line-oriented: the lines are kept until the
/// end of the file, where they are parsed once as one JSON document.
pub(super) struct SettingsParser {
    ctx: ParserCtx,
    lines: Vec<String>,
}

impl SettingsParser {
    pub(super) fn new(ctx: ParserCtx) -> Self {
        Self {
            ctx,
            lines: Vec::new(),
        }
    }
}

impl Parser for SettingsParser {
    fn on_line(&mut self, line: &str) -> Vec<RawEvent> {
        self.lines.push(line.to_string());
        Vec::new()
    }

    fn end(&mut self) -> Vec<RawEvent> {
        let text = self.lines.join("\n");
        let rec = match stored_json(&self.ctx.file, "the settings sidecar", &text) {
            Ok(rec) => rec,
            Err(error) => {
                eprintln!("sessions: {error}");
                return Vec::new();
            }
        };
        if !rec.is_object() {
            return Vec::new();
        }
        let Some(Value::String(ts)) = rec.get("providerLockTimestamp") else {
            return Vec::new();
        };
        // A sidecar without a token usage object reports no totals.
        let usage = rec.get("tokenUsage").filter(|value| value.is_object());
        let mut extra = Map::new();
        extra.insert("kind".into(), Value::from("settings"));
        let provider = match rec.get("apiProviderLock") {
            Some(value) if !value.is_null() => Some(value),
            _ => rec.get("providerLock"),
        };
        prune(&mut extra, "provider", provider);
        vec![RawEvent {
            ts: Some(ts.clone()),
            session_id: self.ctx.session_id.clone(),
            project: self.ctx.project.clone(),
            event_type: "meta".to_string(),
            text: String::new(),
            tool_name: None,
            model: None,
            tokens_in: usage.and_then(|usage| int_field(usage, "inputTokens")),
            tokens_out: usage.and_then(|usage| int_field(usage, "outputTokens")),
            extra,
        }]
    }
}
