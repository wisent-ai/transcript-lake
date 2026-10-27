//! The hooks source as an adapter, for the replay path that reads ready
//! segments the way every other runtime is read: one root, one parser, and a
//! malformed line reported rather than silently dropped.

use std::path::PathBuf;

use serde_json::{Map, Value};

use crate::stream::warn;
use crate::types::{Adapter, Parser, ParserCtx, RawEvent, Reading, SessionEntry};

use super::*;

/// Pseudo-adapter over the adaptive hook decision log, used when no closed-segment
/// ready directory exists. Record shape, from the hooks-rotator telemetry writer:
/// `{ ts (epoch millis), event, id, decision, ms, code, tool, timedOut, infra, reason }`.
/// Downstream SQL relies on `extra.decision` / `extra.event` / `extra.infra` passing
/// through unchanged.
pub fn hooks_adapter() -> Box<dyn Adapter> {
    Box::new(Hooks)
}

struct Hooks;

impl Adapter for Hooks {
    fn runtime(&self) -> &'static str {
        crate::types::HOOKS
    }

    fn roots(&self, home: &Path) -> Vec<PathBuf> {
        crate::adapters::common::existing_root(home.join(".hooks-adaptive"))
    }

    fn list_sessions(&self, root: &Path) -> Vec<SessionEntry> {
        let mut out = Vec::new();
        for name in ["telemetry.prev.jsonl", "telemetry.jsonl"] {
            let file = root.join(name);
            // A log that cannot be examined is said, as an unreadable root
            // is: skipped silently, its events read as never written.
            match crate::lake::paths::present(&file) {
                Ok(true) => out.push(SessionEntry {
                    file,
                    session_id: None,
                    project: None,
                }),
                Ok(false) => {}
                Err(cause) => eprintln!("sessions: {cause}, so it is not read"),
            }
        }
        out
    }

    /// The two telemetry logs this adapter reads, recognised by name. Anything
    /// else under the root — a closed segment, a claim, an acknowledgement —
    /// belongs to the segment path, not to this one.
    fn entry_for(&self, path: &Path) -> Option<SessionEntry> {
        let name = path.file_name()?.to_str()?;
        if !matches!(name, "telemetry.prev.jsonl" | "telemetry.jsonl") {
            return None;
        }
        let root = crate::util::home_dir().join(".hooks-adaptive");
        if path.parent() != Some(root.as_path()) {
            return None;
        }
        Some(SessionEntry {
            file: path.to_path_buf(),
            session_id: None,
            project: None,
        })
    }

    fn read(&self, ctx: ParserCtx) -> Reading {
        Reading::Lines(Box::new(HooksParser { file: ctx.file }))
    }
}

struct HooksParser {
    file: PathBuf,
}

impl Parser for HooksParser {
    /// The frozen parser interface maps a malformed line to zero events; the drop is
    /// still reported on stderr so it is never invisible.
    fn on_line(&mut self, raw: &str) -> Vec<RawEvent> {
        let line = raw.trim();
        if line.is_empty() {
            return Vec::new();
        }
        let rec = match serde_json::from_str::<Value>(line) {
            Ok(value) => value,
            Err(error) => {
                warn(&format!(
                    "{}: dropped malformed telemetry line: {error}",
                    self.file.display()
                ));
                return Vec::new();
            }
        };
        if !rec.is_object() && !rec.is_array() {
            return Vec::new();
        }
        let ts = match rec.get("ts") {
            Some(Value::Number(number)) => {
                // A time that is not a finite number of milliseconds is no
                // moment, and the line is dropped as one without a time.
                let Some(millis) = number.as_f64().filter(|millis| millis.is_finite()) else {
                    return Vec::new();
                };
                // `new Date(ms).toISOString()` threw a RangeError here, which the
                // driver caught per line and reported; the line is still dropped.
                match epoch_iso(millis) {
                    Some(stamp) => Some(stamp),
                    None => {
                        warn(&format!(
                            "{}: dropped telemetry line with an unrepresentable timestamp",
                            self.file.display()
                        ));
                        return Vec::new();
                    }
                }
            }
            Some(Value::String(text)) => Some(text.clone()),
            _ => None,
        };
        let Some(ts) = ts.filter(|value| !value.is_empty()) else {
            return Vec::new();
        };
        // Every field the writer recorded travels in `extra` under its own
        // name, so the decision, event and infra flags downstream SQL reads
        // arrive unchanged; a null field records nothing.
        let mut extra = Map::new();
        if let Value::Object(fields) = &rec {
            for (key, value) in fields {
                if !value.is_null() {
                    extra.insert(key.clone(), value.clone());
                }
            }
        }
        vec![RawEvent {
            ts: Some(ts),
            session_id: first_present(&rec, &["session_id", "sessionId", "session"]),
            project: first_present(&rec, &["project", "cwd"]),
            event_type: "hook_decision".to_string(),
            text: match rec.get("reason") {
                Some(Value::String(reason)) => Some(reason.clone()),
                _ => None,
            },
            tool_name: match rec.get("id") {
                Some(Value::String(id)) => Some(id.clone()),
                _ => None,
            },
            model: None,
            tokens_in: None,
            tokens_out: None,
            extra,
        }]
    }
}

/// The `a ?? b ?? c` chain: the first key that is present and not null wins. A
/// non-string winner is rendered rather than dropped, because session_id is the join
/// key every downstream label and export is grouped by.
pub(super) fn first_present(rec: &Value, keys: &[&str]) -> Option<String> {
    for key in keys {
        let Some(value) = rec.get(*key) else {
            continue;
        };
        if value.is_null() {
            continue;
        }
        return Some(match value.as_str() {
            Some(text) => text.to_string(),
            None => value.to_string(),
        });
    }
    None
}
