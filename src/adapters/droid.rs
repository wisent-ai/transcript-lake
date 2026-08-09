//! Adapter for Factory Droid session transcripts.
//!
//! Source layout: `HOME/.factory/sessions/<uuid>.jsonl` (legacy, flat) and
//! `HOME/.factory/sessions/<encoded-cwd>/<uuid>.jsonl`, each with an optional
//! `<uuid>.settings.json` sidecar (multi-line JSON: providerLock, tokenUsage).
//! Record types verified across a wide sample of real files on this machine:
//!   session_start — first line, no timestamp; legacy files carry only
//!     `{ id, title, owner }`, newer ones add cwd / version / sessionTitle,
//!   message — `{ id, timestamp, parentId, message: { role, content } }` with
//!     roles user | assistant and content blocks text `{ text }`,
//!     tool_use `{ id, name, input }`, tool_result `{ tool_use_id, content }`,
//!     thinking `{ thinking, signature }`; tool results ride inside user-role
//!     records; no per-message usage or model fields exist,
//!   todo_state — `{ timestamp, todos: { todos: [...] } }`,
//!   compaction_state — `{ timestamp, summaryText }`.
//! Unknown record or block types map to meta events with a type tag in extra
//! rather than being dropped. Sidecars contribute at most one meta event.
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::types::{Adapter, Parser, ParserCtx, RawEvent, SessionEntry};

const TEXT_CAP: usize = 65536;
const PENDING_CAP: usize = 64;
const JSONL_EXT: &str = ".jsonl";
const SETTINGS_EXT: &str = ".settings.json";

pub struct Droid;

impl Adapter for Droid {
    fn runtime(&self) -> &'static str {
        "droid"
    }

    fn roots(&self, home: &Path) -> Vec<PathBuf> {
        let base = home.join(".factory").join("sessions");
        let Some(entries) = read_dirents(&base) else {
            return Vec::new();
        };
        let mut dirs = vec![base.clone()];
        for (name, file_type) in entries {
            if file_type.is_dir() {
                dirs.push(base.join(name));
            }
        }
        dirs
    }

    fn list_sessions(&self, root: &Path) -> Vec<SessionEntry> {
        let Some(entries) = read_dirents(root) else {
            return Vec::new();
        };
        let project = root
            .file_name()
            .map(|name| name.to_string_lossy().to_string())
            .and_then(|name| decode_project(&name));
        let mut sessions = Vec::new();
        for (name, file_type) in entries {
            if !file_type.is_file() {
                continue;
            }
            let extension = if name.ends_with(SETTINGS_EXT) {
                SETTINGS_EXT
            } else if name.ends_with(JSONL_EXT) {
                JSONL_EXT
            } else {
                continue;
            };
            let session_id = name[..name.len() - extension.len()].to_string();
            sessions.push(SessionEntry {
                file: root.join(&name),
                session_id: Some(session_id),
                project: project.clone(),
            });
        }
        sessions
    }

    fn parser(&self, ctx: ParserCtx) -> Box<dyn Parser> {
        if ctx.file.to_string_lossy().ends_with(SETTINGS_EXT) {
            return Box::new(SettingsParser { ctx, lines: Vec::new() });
        }
        Box::new(TranscriptParser {
            project: ctx.project.clone(),
            session_id: ctx.session_id.clone(),
            last_ts: None,
            pending: Vec::new(),
            ready: false,
        })
    }
}

/// Every directory read failure is tolerated here, exactly as the previous
/// implementation's bare `catch { return [] }` did. Names come back in byte order,
/// because Node's `readdirSync` sorts with strcmp and the walk order is observable in
/// `sources` output and in which partition file a record lands in.
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

/// Best-effort decode of '-Users-name-...' directory names; lossy for real
/// dashes in path segments, so session_start cwd overrides it when present.
fn decode_project(name: &str) -> Option<String> {
    if !name.starts_with('-') {
        return None;
    }
    Some(name.replace('-', "/"))
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

/// Sidecar settings files are whole-file JSON, accumulated line by line and
/// parsed once at end(); they yield at most one meta event with token totals.
struct SettingsParser {
    ctx: ParserCtx,
    lines: Vec<String>,
}

impl Parser for SettingsParser {
    fn on_line(&mut self, line: &str) -> Vec<RawEvent> {
        self.lines.push(line.to_string());
        Vec::new()
    }

    fn end(&mut self) -> Vec<RawEvent> {
        let Ok(rec) = serde_json::from_str::<Value>(&self.lines.join("\n")) else {
            return Vec::new();
        };
        if !rec.is_object() {
            return Vec::new();
        }
        let Some(Value::String(ts)) = rec.get("providerLockTimestamp") else {
            return Vec::new();
        };
        let empty = Value::Object(Map::new());
        let usage = rec.get("tokenUsage").filter(|value| value.is_object()).unwrap_or(&empty);
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
            tokens_in: usage.get("inputTokens").and_then(number),
            tokens_out: usage.get("outputTokens").and_then(number),
            extra,
        }]
    }
}

/// A JSON value that is a number, as `typeof x === 'number'` accepted it.
fn number(value: &Value) -> Option<i64> {
    value.as_f64().filter(|raw| raw.is_finite()).map(|raw| raw as i64)
}

struct TranscriptParser {
    project: Option<String>,
    session_id: Option<String>,
    last_ts: Option<String>,
    pending: Vec<RawEvent>,
    ready: bool,
}

impl Parser for TranscriptParser {
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
        // session_start carries no timestamp; hold events until the first
        // stamped record so its ts (and any late cwd) can be backfilled.
        if self.last_ts.is_some() || self.pending.len() > PENDING_CAP {
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

impl TranscriptParser {
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
            _ => None,
        };
        if let Some(ts) = &ts {
            self.last_ts = Some(ts.clone());
        }
        Some(ts.or_else(|| self.last_ts.clone()))
    }

    fn handle(&mut self, rec: &Value) -> Option<Vec<RawEvent>> {
        let ts = self.stamp(rec)?;
        let record_type = rec.get("type").and_then(Value::as_str).unwrap_or_default().to_string();
        if record_type == "session_start" {
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
            let title = match rec.get("sessionTitle") {
                Some(Value::String(title)) if !title.is_empty() => title.clone(),
                _ => match rec.get("title") {
                    Some(Value::String(title)) => title.clone(),
                    _ => String::new(),
                },
            };
            let mut extra = Map::new();
            extra.insert("kind".into(), Value::from(record_type));
            prune(&mut extra, "version", rec.get("version"));
            let mut event = self.make(ts.as_ref(), "meta", &title);
            event.extra = extra;
            return Some(vec![event]);
        }
        if record_type == "message" {
            return Some(self.message_events(rec, ts.as_ref()));
        }
        if record_type == "todo_state" {
            let count = rec
                .get("todos")
                .and_then(|box_value| box_value.get("todos"))
                .and_then(Value::as_array)
                .map_or(0, Vec::len);
            let mut event = self.make(ts.as_ref(), "meta", "");
            event.extra.insert("kind".into(), Value::from(record_type));
            event.extra.insert("todo_count".into(), Value::from(count));
            return Some(vec![event]);
        }
        if record_type == "compaction_state" {
            let summary = match rec.get("summaryText") {
                Some(Value::String(text)) => text.clone(),
                _ => String::new(),
            };
            let mut event = self.make(ts.as_ref(), "meta", &summary);
            event.extra.insert("kind".into(), Value::from(record_type));
            return Some(vec![event]);
        }
        let mut event = self.make(ts.as_ref(), "meta", "");
        event.extra.insert("kind".into(), Value::from("unknown"));
        event.extra.insert("droid_type".into(), Value::from(js_string(rec.get("type"))));
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
                Some("tool_use") => {
                    self.flush_text(&mut events, &mut buffer, ts, text_type, &role);
                    let mut event = self.make(ts, "tool_call", "");
                    event.tool_name = match block.get("name") {
                        Some(Value::String(name)) => Some(name.clone()),
                        _ => None,
                    };
                    prune(&mut event.extra, "call_id", block.get("id"));
                    events.push(event);
                }
                Some("tool_result") => {
                    self.flush_text(&mut events, &mut buffer, ts, text_type, &role);
                    let mut event = self.make(ts, "tool_result", &text_of(block.get("content")));
                    prune(&mut event.extra, "call_id", block.get("tool_use_id"));
                    events.push(event);
                }
                _ => {
                    self.flush_text(&mut events, &mut buffer, ts, text_type, &role);
                    let mut event = self.make(ts, "meta", "");
                    event.extra.insert("kind".into(), Value::from("block"));
                    event
                        .extra
                        .insert("droid_block".into(), Value::from(js_string(block.get("type"))));
                    events.push(event);
                }
            }
        }
        self.flush_text(&mut events, &mut buffer, ts, text_type, &role);
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

/// `new Date(ms).toISOString()`: a value outside the representable range threw there
/// and the driver dropped the line, which is what `None` does here.
fn epoch_iso(millis: f64) -> Option<String> {
    if !millis.is_finite() {
        return None;
    }
    let stamp = chrono::DateTime::from_timestamp_millis(millis as i64)?;
    Some(stamp.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
}
