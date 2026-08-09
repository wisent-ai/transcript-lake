//! Adapter for Oh My Pi (omp) agent session transcripts.
//!
//! Source layout: `HOME/.omp/agent/sessions/<encoded-cwd>/<stamp>_<uuid>.jsonl`;
//! a sibling directory with the same stem holds non-transcript artifacts and
//! is skipped. Typed lines verified on real files from this machine:
//!   session (id, cwd, version), title / title_change (title, updatedAt),
//!   model_change (model), thinking_level_change (thinkingLevel),
//!   message (`{ id, parentId, timestamp, message: { role, content } }`) with
//!     roles user | assistant | toolResult | developer and content blocks
//!     text `{ text }` | thinking `{ thinking }` | toolCall `{ id, name, arguments }`;
//!     toolResult messages carry toolName, toolCallId, isError, content blocks;
//!     assistant messages carry model plus usage `{ input, output, ... }`,
//!   custom_message (customType, content string), custom (customType, data),
//!   compaction (summary, tokensBefore, firstKeptEntryId).
//! Unknown record or block types become meta events tagged in extra.
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::types::{Adapter, Parser, ParserCtx, RawEvent, SessionEntry};

const TEXT_CAP: usize = 65536;
const PENDING_CAP: usize = 64;
const JSONL_EXT: &str = ".jsonl";

pub struct Omp;

impl Adapter for Omp {
    fn runtime(&self) -> &'static str {
        "omp"
    }

    fn roots(&self, home: &Path) -> Vec<PathBuf> {
        let base = home.join(".omp").join("agent").join("sessions");
        let Some(entries) = read_dirents(&base) else {
            return Vec::new();
        };
        entries
            .into_iter()
            .filter(|(_, file_type)| file_type.is_dir())
            .map(|(name, _)| base.join(name))
            .collect()
    }

    fn list_sessions(&self, root: &Path) -> Vec<SessionEntry> {
        let Some(entries) = read_dirents(root) else {
            return Vec::new();
        };
        let mut sessions = Vec::new();
        for (name, file_type) in entries {
            if !file_type.is_file() || !name.ends_with(JSONL_EXT) {
                continue;
            }
            let stem = &name[..name.len() - JSONL_EXT.len()];
            // Encoded directory names are home-relative and dash-mangled for omp, so
            // the parser recovers the real project from the session line cwd instead.
            let session_id = match stem.find('_') {
                Some(at) => stem[at + 1..].to_string(),
                None => stem.to_string(),
            };
            sessions.push(SessionEntry {
                file: root.join(&name),
                session_id: Some(session_id),
                project: None,
            });
        }
        sessions
    }

    fn parser(&self, ctx: ParserCtx) -> Box<dyn Parser> {
        Box::new(OmpParser {
            project: ctx.project,
            session_id: ctx.session_id,
            last_ts: None,
            model: None,
            pending: Vec::new(),
            ready: false,
        })
    }
}

/// Every directory read failure is tolerated here, exactly as the previous
/// implementation's bare `catch { return [] }` did. Names come back in byte order,
/// because Node's `readdirSync` sorts with strcmp and the walk order is observable.
fn read_dirents(dir: &Path) -> Option<Vec<(String, fs::FileType)>> {
    let iter = fs::read_dir(dir).ok()?;
    let mut out = Vec::new();
    for entry in iter.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        out.push((entry.file_name().to_string_lossy().to_string(), file_type));
    }
    out.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    Some(out)
}

fn clip(value: &str) -> String {
    let mut units = 0usize;
    for (index, character) in value.char_indices() {
        let width = character.len_utf16();
        if units + width > TEXT_CAP {
            return value[..index].to_string();
        }
        units += width;
    }
    value.to_string()
}

fn text_of(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(items)) => {
            let mut parts = Vec::new();
            for item in items {
                match item {
                    Value::String(text) => parts.push(text.clone()),
                    Value::Object(map) => {
                        if let Some(Value::String(text)) = map.get("text") {
                            parts.push(text.clone());
                        }
                    }
                    _ => {}
                }
            }
            parts.join("\n")
        }
        _ => String::new(),
    }
}

/// `prune`: a key present with a non-null value survives, everything else is dropped.
fn prune(extra: &mut Map<String, Value>, key: &str, value: Option<&Value>) {
    if let Some(value) = value {
        if !value.is_null() {
            extra.insert(key.to_string(), value.clone());
        }
    }
}

/// JS `String(value)` for the shapes a record or block type can take.
fn js_string(value: Option<&Value>) -> String {
    match value {
        None => "undefined".to_string(),
        Some(Value::Null) => "null".to_string(),
        Some(Value::Bool(flag)) => flag.to_string(),
        Some(Value::String(text)) => text.clone(),
        Some(Value::Number(number)) => number.to_string(),
        Some(Value::Array(items)) => items
            .iter()
            .map(|item| match item {
                Value::Null => String::new(),
                other => js_string(Some(other)),
            })
            .collect::<Vec<_>>()
            .join(","),
        Some(Value::Object(_)) => "[object Object]".to_string(),
    }
}

/// `new Date(ms).toISOString()`: a value outside the representable range threw there
/// and the driver dropped the line, which is what `None` does here.
fn epoch_iso(millis: f64) -> Option<String> {
    if !millis.is_finite() {
        return None;
    }
    let stamp = chrono::DateTime::from_timestamp_millis(millis as i64)?;
    Some(stamp.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
}

/// A JSON value that is a number, as `typeof x === 'number'` accepted it.
fn number(value: &Value) -> Option<i64> {
    value.as_f64().filter(|raw| raw.is_finite()).map(|raw| raw as i64)
}

struct OmpParser {
    project: Option<String>,
    session_id: Option<String>,
    last_ts: Option<String>,
    model: Option<String>,
    pending: Vec<RawEvent>,
    ready: bool,
}

impl Parser for OmpParser {
    fn on_line(&mut self, line: &str) -> Vec<RawEvent> {
        if line.trim().is_empty() {
            return Vec::new();
        }
        let Ok(rec) = serde_json::from_str::<Value>(line) else {
            return Vec::new();
        };
        if !rec.is_object() {
            return Vec::new();
        }
        let Some(events) = self.handle(&rec) else {
            return Vec::new();
        };
        if self.ready {
            return events;
        }
        self.pending.extend(events);
        if rec.get("type").and_then(Value::as_str) == Some("session")
            || self.pending.len() > PENDING_CAP
        {
            self.ready = true;
            return self.flush_pending();
        }
        Vec::new()
    }

    fn end(&mut self) -> Vec<RawEvent> {
        self.ready = true;
        self.flush_pending()
    }
}

impl OmpParser {
    fn make(&self, ts: Option<&String>, event_type: &str, text: &str) -> RawEvent {
        RawEvent {
            ts: ts.cloned(),
            session_id: self.session_id.clone(),
            project: self.project.clone(),
            event_type: event_type.to_string(),
            text: clip(text),
            ..RawEvent::default()
        }
    }

    /// `None` means the record's epoch timestamp is unrepresentable, which threw
    /// inside `new Date(ms).toISOString()` and dropped the whole line.
    fn stamp(&mut self, rec: &Value) -> Option<Option<String>> {
        let ts = match rec.get("timestamp") {
            Some(Value::String(text)) => Some(text.clone()),
            Some(Value::Number(number)) => Some(epoch_iso(number.as_f64()?)?),
            _ => match rec.get("updatedAt") {
                Some(Value::String(text)) => Some(text.clone()),
                _ => None,
            },
        };
        if let Some(ts) = &ts {
            self.last_ts = Some(ts.clone());
        }
        Some(ts.or_else(|| self.last_ts.clone()))
    }

    fn handle(&mut self, rec: &Value) -> Option<Vec<RawEvent>> {
        let ts = self.stamp(rec)?;
        let record_type = rec.get("type").and_then(Value::as_str).unwrap_or_default().to_string();
        if record_type == "session" {
            if let Some(Value::String(cwd)) = rec.get("cwd") {
                if !cwd.is_empty() {
                    self.project = Some(cwd.clone());
                }
            }
            if let Some(Value::String(id)) = rec.get("id") {
                if !id.is_empty() {
                    self.session_id = Some(id.clone());
                }
            }
            let mut extra = Map::new();
            extra.insert("kind".into(), Value::from(record_type));
            prune(&mut extra, "version", rec.get("version"));
            let mut event = self.make(ts.as_ref(), "meta", "");
            event.extra = extra;
            return Some(vec![event]);
        }
        if record_type == "message" {
            return Some(self.message_events(rec, ts.as_ref()));
        }
        if record_type == "title" || record_type == "title_change" {
            let title = match rec.get("title") {
                Some(Value::String(title)) => title.clone(),
                _ => String::new(),
            };
            let mut event = self.make(ts.as_ref(), "meta", &title);
            event.extra.insert("kind".into(), Value::from(record_type));
            return Some(vec![event]);
        }
        if record_type == "model_change" {
            if let Some(Value::String(model)) = rec.get("model") {
                self.model = Some(model.clone());
            }
            let mut event = self.make(ts.as_ref(), "meta", "");
            event.extra.insert("kind".into(), Value::from(record_type));
            event.model = self.model.clone();
            return Some(vec![event]);
        }
        if record_type == "thinking_level_change" {
            let mut extra = Map::new();
            extra.insert("kind".into(), Value::from(record_type));
            prune(&mut extra, "level", rec.get("thinkingLevel"));
            let mut event = self.make(ts.as_ref(), "meta", "");
            event.extra = extra;
            return Some(vec![event]);
        }
        if record_type == "compaction" {
            let summary = match rec.get("summary") {
                Some(Value::String(text)) => text.clone(),
                _ => String::new(),
            };
            let mut extra = Map::new();
            extra.insert("kind".into(), Value::from(record_type));
            prune(&mut extra, "tokens_before", rec.get("tokensBefore"));
            prune(&mut extra, "first_kept_entry", rec.get("firstKeptEntryId"));
            let mut event = self.make(ts.as_ref(), "meta", &summary);
            event.extra = extra;
            return Some(vec![event]);
        }
        if record_type == "custom_message" || record_type == "custom" {
            let mut extra = Map::new();
            extra.insert("kind".into(), Value::from(record_type));
            prune(&mut extra, "custom_type", rec.get("customType"));
            let mut event = self.make(ts.as_ref(), "meta", &text_of(rec.get("content")));
            event.extra = extra;
            return Some(vec![event]);
        }
        let mut event = self.make(ts.as_ref(), "meta", "");
        event.extra.insert("kind".into(), Value::from("unknown"));
        event.extra.insert("omp_type".into(), Value::from(js_string(rec.get("type"))));
        Some(vec![event])
    }

    fn message_events(&self, rec: &Value, ts: Option<&String>) -> Vec<RawEvent> {
        let Some(msg) = rec.get("message").filter(|value| value.is_object()) else {
            let mut event = self.make(ts, "meta", "");
            event.extra.insert("kind".into(), Value::from("message"));
            return vec![event];
        };
        let role = match msg.get("role") {
            Some(Value::String(role)) => role.clone(),
            _ => "unknown".to_string(),
        };
        if role == "toolResult" {
            let mut event = self.make(ts, "tool_result", &text_of(msg.get("content")));
            event.tool_name = match msg.get("toolName") {
                Some(Value::String(name)) => Some(name.clone()),
                _ => None,
            };
            prune(&mut event.extra, "call_id", msg.get("toolCallId"));
            if msg.get("isError") == Some(&Value::Bool(true)) {
                event.extra.insert("is_error".into(), Value::Bool(true));
            }
            return vec![event];
        }
        let text_type = match role.as_str() {
            "user" => "user",
            "assistant" => "assistant",
            _ => "meta",
        };
        let mut events: Vec<RawEvent> = Vec::new();
        let mut buffer: Vec<String> = Vec::new();
        let owned;
        let blocks: &[Value] = match msg.get("content") {
            Some(Value::Array(blocks)) => blocks,
            other => {
                owned = vec![serde_json::json!({ "type": "text", "text": text_of(other) })];
                &owned
            }
        };
        for block in blocks {
            if !block.is_object() {
                continue;
            }
            match block.get("type").and_then(Value::as_str) {
                Some("text") => {
                    buffer.push(match block.get("text") {
                        Some(Value::String(text)) => text.clone(),
                        _ => String::new(),
                    });
                }
                Some("thinking") => {
                    self.flush_text(&mut events, &mut buffer, ts, text_type, &role);
                    let thinking = match block.get("thinking") {
                        Some(Value::String(text)) => text.clone(),
                        _ => String::new(),
                    };
                    events.push(self.make(ts, "thinking", &thinking));
                }
                Some("toolCall") => {
                    self.flush_text(&mut events, &mut buffer, ts, text_type, &role);
                    let mut event = self.make(ts, "tool_call", "");
                    event.tool_name = match block.get("name") {
                        Some(Value::String(name)) => Some(name.clone()),
                        _ => None,
                    };
                    prune(&mut event.extra, "call_id", block.get("id"));
                    events.push(event);
                }
                _ => {
                    self.flush_text(&mut events, &mut buffer, ts, text_type, &role);
                    let mut event = self.make(ts, "meta", "");
                    event.extra.insert("kind".into(), Value::from("block"));
                    event
                        .extra
                        .insert("omp_block".into(), Value::from(js_string(block.get("type"))));
                    events.push(event);
                }
            }
        }
        self.flush_text(&mut events, &mut buffer, ts, text_type, &role);
        if role == "assistant" {
            let model = match msg.get("model") {
                Some(Value::String(model)) => Some(model.clone()),
                _ => self.model.clone(),
            };
            for event in &mut events {
                event.model = model.clone();
            }
            if let Some(head) = events.first_mut() {
                if let Some(usage) = msg.get("usage").filter(|value| value.is_object()) {
                    if let Some(tokens) = usage.get("input").and_then(number) {
                        head.tokens_in = Some(tokens);
                    }
                    if let Some(tokens) = usage.get("output").and_then(number) {
                        head.tokens_out = Some(tokens);
                    }
                }
            }
        }
        events
    }

    fn flush_text(
        &self,
        events: &mut Vec<RawEvent>,
        buffer: &mut Vec<String>,
        ts: Option<&String>,
        text_type: &str,
        role: &str,
    ) {
        if buffer.is_empty() {
            return;
        }
        let mut event = self.make(ts, text_type, &buffer.join("\n"));
        if text_type == "meta" {
            event.extra.insert("kind".into(), Value::from("message"));
            event.extra.insert("role".into(), Value::from(role));
        }
        events.push(event);
        buffer.clear();
    }

    fn flush_pending(&mut self) -> Vec<RawEvent> {
        let flushed = std::mem::take(&mut self.pending);
        flushed
            .into_iter()
            .filter_map(|mut event| {
                if event.project.is_none() {
                    event.project = self.project.clone();
                }
                event.session_id = self.session_id.clone();
                if event.ts.is_none() {
                    event.ts = self.last_ts.clone();
                }
                event.ts.is_some().then_some(event)
            })
            .collect()
    }
}
