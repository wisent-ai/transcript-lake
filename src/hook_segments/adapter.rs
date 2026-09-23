//! The hooks source as an adapter, for the replay path that reads ready
//! segments the way every other runtime is read: one root, one parser, and a
//! malformed line reported rather than silently dropped.

use std::path::PathBuf;

use serde_json::{Map, Value};

use crate::stream::warn;
use crate::types::{Adapter, Parser, ParserCtx, RawEvent, SessionEntry};

use super::*;

/// Pseudo-adapter over the adaptive hook decision log, used when no closed-segment
/// ready directory exists. Record shape, from the hooks-rotator telemetry writer:
/// `{ ts (epoch millis), event, id, decision, ms, code, tool, timedOut, infra, reason }`.
/// Downstream SQL relies on `extra.decision` / `extra.event` / `extra.infra` passing
/// through unchanged.
pub fn hooks_adapter() -> Box<dyn Adapter> {
    Box::new(Hooks)
}

const PICK: [&str; 7] = [
    "event", "decision", "tool", "code", "ms", "timedOut", "infra",
];

struct Hooks;

impl Adapter for Hooks {
    fn runtime(&self) -> &'static str {
        crate::types::HOOKS
    }

    fn roots(&self, home: &Path) -> Vec<PathBuf> {
        let dir = home.join(".hooks-adaptive");
        if dir.exists() {
            return vec![dir];
        }
        Vec::new()
    }

    fn list_sessions(&self, root: &Path) -> Vec<SessionEntry> {
        let mut out = Vec::new();
        for name in ["telemetry.prev.jsonl", "telemetry.jsonl"] {
            let file = root.join(name);
            if file.exists() {
                out.push(SessionEntry {
                    file,
                    session_id: None,
                    project: None,
                });
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

    fn parser(&self, ctx: ParserCtx) -> Box<dyn Parser> {
        Box::new(HooksParser { file: ctx.file })
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
                let millis = number.as_f64().unwrap_or(f64::NAN);
                if !millis.is_finite() {
                    return Vec::new();
                }
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
        let mut extra = Map::new();
        for key in PICK {
            if let Some(value) = rec.get(key) {
                if !value.is_null() {
                    extra.insert(key.to_string(), value.clone());
                }
            }
        }
        vec![RawEvent {
            ts: Some(ts),
            session_id: first_present(&rec, &["session_id", "sessionId", "session"]),
            project: first_present(&rec, &["project", "cwd"]),
            event_type: "hook_decision".to_string(),
            text: match rec.get("reason") {
                Some(Value::String(reason)) => reason.clone(),
                _ => String::new(),
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
