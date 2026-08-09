//! Adapter: Codex CLI rollouts — `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`
//!
//! Frozen interface: `runtime`, `roots(home)`, `list_sessions(root)`, `parser(ctx)`.
//! Adapters emit UNMASKED text (the ingest driver masks) and never do IO in `on_line`;
//! malformed lines are tolerated silently. Envelope per line:
//! `{ timestamp, type, payload }`. Verified on live files, old and current CLI
//! versions: content is duplicated between the response_item stream and event_msg
//! (user_message / agent_message / agent_reasoning). To avoid double counting we take
//! user turns from event_msg/user_message (response_item role=user also carries
//! injected environment context) and everything else from response_item records.
use std::fs;
use std::path::{Path, PathBuf};

use regex::Regex;
use serde_json::{Map, Value};

use crate::types::{Adapter, Parser, ParserCtx, RawEvent, SessionEntry};

const TEXT_CAP: usize = 65536;

/// Filenames look like `rollout-<ISO-stamp>-<uuid>.jsonl`; the uuid is the session id.
static UUID_RE: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
    Regex::new("(?i)[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}").expect("uuid pattern")
});

pub struct Codex;

impl Adapter for Codex {
    fn runtime(&self) -> &'static str {
        "codex"
    }

    fn roots(&self, home: &Path) -> Vec<PathBuf> {
        let dir = home.join(".codex").join("sessions");
        if dir.exists() {
            return vec![dir];
        }
        Vec::new()
    }

    fn list_sessions(&self, root: &Path) -> Vec<SessionEntry> {
        let mut sessions = Vec::new();
        for year in subdirs(root) {
            for month in subdirs(&year) {
                for day in subdirs(&month) {
                    for (name, file_type) in read_dirents(&day) {
                        if !file_type.is_file() {
                            continue;
                        }
                        if let Some(session) = day_entry(&day, &name) {
                            sessions.push(session);
                        }
                    }
                }
            }
        }
        sessions
    }

    fn entry_for(&self, path: &Path) -> Option<SessionEntry> {
        let name = path.file_name()?.to_string_lossy();
        let day = path.parent()?;
        let month = day.parent()?;
        let year = month.parent()?;
        let home = crate::util::home_dir();
        if !self.roots(&home).iter().any(|root| year.parent() == Some(root.as_path())) {
            return None;
        }
        // The date nesting `subdirs` walks is exactly three levels deep and each level
        // must be a real directory, so a rollout parked anywhere else is not ours.
        for dir in [year, month, day] {
            if !dirent_type(dir)?.is_dir() {
                return None;
            }
        }
        if !dirent_type(path)?.is_file() {
            return None;
        }
        day_entry(day, &name)
    }

    fn parser(&self, ctx: ParserCtx) -> Box<dyn Parser> {
        Box::new(CodexParser {
            session_id: ctx.session_id.filter(|value| !value.is_empty()),
            project: ctx.project.filter(|value| !value.is_empty()),
            model: None,
            last_ts: None,
        })
    }
}

/// A directory may vanish between scan and read; that is not an error state worth
/// failing ingestion over. Anything else (permissions, IO) was fatal in the previous
/// implementation, which could throw out of `listSessions`; the frozen Rust signature
/// cannot, so it is reported on stderr with the driver's own prefix instead. Names come
/// back in byte order, because Node's `readdirSync` sorts with strcmp and the walk
/// order decides which partition file a record lands in.
fn read_dirents(dir: &Path) -> Vec<(String, fs::FileType)> {
    let iter = match fs::read_dir(dir) {
        Ok(iter) => iter,
        Err(error) => {
            // ENOENT and ENOTDIR: the tree moved under us.
            if !matches!(error.raw_os_error(), Some(2) | Some(20)) {
                eprintln!("ingest: listSessions failed under {}: {error}", dir.display());
            }
            return Vec::new();
        }
    };
    let mut out = Vec::new();
    for entry in iter.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        out.push((entry.file_name().to_string_lossy().to_string(), file_type));
    }
    out.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    out
}

fn subdirs(dir: &Path) -> Vec<PathBuf> {
    read_dirents(dir)
        .into_iter()
        .filter(|(_, file_type)| file_type.is_dir())
        .map(|(name, _)| dir.join(name))
        .collect()
}

/// The type `read_dirents` would have reported for a path the caller already knows,
/// taken without following a symlink so that an entry the scan skips is skipped here
/// too. A path that vanished between notification and read has no type at all.
fn dirent_type(path: &Path) -> Option<fs::FileType> {
    fs::symlink_metadata(path).ok().map(|meta| meta.file_type())
}

/// The entry a scan yields for one name inside a day directory: `rollout-*.jsonl` only,
/// with the uuid in the stem as the session id. Shared with `entry_for` so one known
/// path and a full scan cannot disagree about a file.
fn day_entry(day: &Path, name: &str) -> Option<SessionEntry> {
    if !name.starts_with("rollout-") || !name.ends_with(".jsonl") {
        return None;
    }
    Some(SessionEntry {
        file: day.join(name),
        session_id: Some(session_id_from_name(name)),
        project: None,
    })
}

fn session_id_from_name(name: &str) -> String {
    let stem = name.strip_suffix(".jsonl").unwrap_or(name);
    match UUID_RE.find(stem) {
        Some(found) => found.as_str().to_string(),
        None => stem.to_string(),
    }
}

/// `String.prototype.slice(0, 65536)`, which counts UTF-16 code units. A cut that would
/// land inside a surrogate pair stops before it rather than emitting a lone surrogate.
fn cap(value: &str) -> String {
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

/// A JSON string field, when present and non-empty.
fn text_field(value: &Value, key: &str) -> Option<String> {
    match value.get(key) {
        Some(Value::String(text)) if !text.is_empty() => Some(text.clone()),
        _ => None,
    }
}

/// A finite JSON number field, defaulting to zero exactly as `num(x) || 0` did.
fn num_or_zero(value: &Value, key: &str) -> i64 {
    value
        .get(key)
        .and_then(Value::as_f64)
        .filter(|raw| raw.is_finite())
        .map_or(0, |raw| raw as i64)
}

struct CodexParser {
    session_id: Option<String>,
    project: Option<String>,
    model: Option<String>,
    last_ts: Option<String>,
}

impl Parser for CodexParser {
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
        self.map_record(&rec)
    }
}

impl CodexParser {
    fn map_record(&mut self, rec: &Value) -> Vec<RawEvent> {
        if let Some(timestamp) = text_field(rec, "timestamp") {
            self.last_ts = Some(timestamp);
        }
        let Some(ts) = self.last_ts.clone() else {
            return Vec::new();
        };
        let Some(payload) = rec.get("payload").filter(|value| value.is_object()).cloned() else {
            return Vec::new();
        };
        let record_type = rec.get("type").and_then(Value::as_str).unwrap_or_default().to_string();
        if record_type == "session_meta" {
            if let Some(id) = text_field(&payload, "id") {
                self.session_id = Some(id);
            }
            if let Some(cwd) = text_field(&payload, "cwd") {
                self.project = Some(cwd);
            }
            let make = self.maker(&ts);
            let mut extra = Map::new();
            extra.insert("kind".into(), Value::from("session_meta"));
            if let Some(originator) = text_field(&payload, "originator") {
                extra.insert("originator".into(), Value::from(originator));
            }
            if let Some(version) = text_field(&payload, "cli_version") {
                extra.insert("cli_version".into(), Value::from(version));
            }
            if let Some(source) = text_field(&payload, "source") {
                extra.insert("source".into(), Value::from(source));
            }
            let mut event = make("meta", "");
            event.extra = extra;
            return vec![event];
        }
        if record_type == "turn_context" {
            // Pure configuration: refresh session state, emit nothing.
            if let Some(model) = text_field(&payload, "model") {
                self.model = Some(model);
            }
            if let Some(cwd) = text_field(&payload, "cwd") {
                self.project = Some(cwd);
            }
            return Vec::new();
        }
        if record_type == "event_msg" {
            return self.map_event_msg(&payload, &ts);
        }
        if record_type == "response_item" {
            return self.map_response_item(&payload, &ts);
        }
        if record_type == "compacted" {
            let make = self.maker(&ts);
            let text = text_field(&payload, "message").unwrap_or_default();
            let mut event = make("meta", &text);
            event.extra.insert("kind".into(), Value::from("compacted"));
            return vec![event];
        }
        Vec::new()
    }

    fn maker(&self, ts: &str) -> impl Fn(&str, &str) -> RawEvent {
        let ts = ts.to_string();
        let session_id = self.session_id.clone();
        let project = self.project.clone();
        move |event_type: &str, text: &str| RawEvent {
            ts: Some(ts.clone()),
            session_id: session_id.clone(),
            project: project.clone(),
            event_type: event_type.to_string(),
            text: cap(text),
            ..RawEvent::default()
        }
    }

    fn map_event_msg(&mut self, payload: &Value, ts: &str) -> Vec<RawEvent> {
        let make = self.maker(ts);
        match payload.get("type").and_then(Value::as_str) {
            Some("user_message") => {
                let text = text_field(payload, "message").unwrap_or_default();
                vec![make("user", &text)]
            }
            Some("token_count") => {
                let Some(info) = payload.get("info").filter(|value| value.is_object()) else {
                    return Vec::new();
                };
                let Some(usage) =
                    info.get("last_token_usage").filter(|value| value.is_object())
                else {
                    return Vec::new();
                };
                let total_input = num_or_zero(usage, "input_tokens");
                let cached_input = num_or_zero(usage, "cached_input_tokens");
                let output = num_or_zero(usage, "output_tokens");
                let reasoning_output = num_or_zero(usage, "reasoning_output_tokens");
                if total_input + output + reasoning_output == 0 {
                    return Vec::new();
                }
                let mut event = make("meta", "");
                event.model = self.model.clone();
                event.tokens_in = Some(total_input);
                event.tokens_out = Some(output + reasoning_output);
                event.extra.insert("kind".into(), Value::from("token_count"));
                event.extra.insert(
                    "input_non_cached_tokens".into(),
                    Value::from((total_input - cached_input).max(0)),
                );
                event.extra.insert("cache_creation_tokens".into(), Value::from(0));
                event.extra.insert("cache_read_tokens".into(), Value::from(cached_input));
                vec![event]
            }
            // agent_message / agent_reasoning duplicate response_item content;
            // task_started, task_complete and the rest are turn bookkeeping. All dropped.
            _ => Vec::new(),
        }
    }

    fn map_response_item(&mut self, payload: &Value, ts: &str) -> Vec<RawEvent> {
        let Some(sub) = text_field(payload, "type") else {
            return Vec::new();
        };
        let make = self.maker(ts);
        if sub == "message" {
            return self.map_message_item(payload, ts);
        }
        if sub == "reasoning" {
            // summary carries the readable text; encrypted_content is opaque and skipped.
            let mut parts = Vec::new();
            if let Some(Value::Array(items)) = payload.get("summary") {
                for item in items {
                    if let Some(Value::String(text)) = item.get("text") {
                        if !text.trim().is_empty() {
                            parts.push(text.clone());
                        }
                    }
                }
            }
            let text = parts.join("\n");
            if text.is_empty() {
                return Vec::new();
            }
            let mut event = make("thinking", &text);
            event.model = self.model.clone();
            return vec![event];
        }
        if sub.ends_with("_call_output") {
            let mut extra = Map::new();
            extra.insert("kind".into(), Value::from(sub.clone()));
            if let Some(call_id) = text_field(payload, "call_id") {
                extra.insert("call_id".into(), Value::from(call_id));
            }
            let mut event = make("tool_result", &output_text(payload.get("output")));
            event.extra = extra;
            return vec![event];
        }
        if sub.ends_with("_call") {
            let mut args = text_field(payload, "arguments")
                .or_else(|| text_field(payload, "input"))
                .unwrap_or_default();
            if args.is_empty() {
                if let Some(action) = payload.get("action") {
                    args = serde_json::to_string(action).unwrap_or_default();
                }
            }
            let mut extra = Map::new();
            extra.insert("kind".into(), Value::from(sub.clone()));
            if let Some(call_id) = text_field(payload, "call_id") {
                extra.insert("call_id".into(), Value::from(call_id));
            }
            let tool_name = text_field(payload, "name").unwrap_or(sub);
            let mut event = make("tool_call", &args);
            event.tool_name = Some(tool_name);
            event.model = self.model.clone();
            event.extra = extra;
            return vec![event];
        }
        Vec::new()
    }

    fn map_message_item(&mut self, payload: &Value, ts: &str) -> Vec<RawEvent> {
        let Some(role) = text_field(payload, "role") else {
            return Vec::new();
        };
        // Role user duplicates event_msg/user_message and additionally carries injected
        // environment/permission context — dropped here to keep user turns clean.
        if role == "user" {
            return Vec::new();
        }
        let mut parts = Vec::new();
        if let Some(Value::Array(blocks)) = payload.get("content") {
            for block in blocks {
                if !block.is_object() {
                    continue;
                }
                if let Some(Value::String(text)) = block.get("text") {
                    if !text.trim().is_empty() {
                        parts.push(text.clone());
                    }
                }
            }
        }
        let text = parts.join("\n");
        let make = self.maker(ts);
        if role == "assistant" {
            if text.is_empty() {
                return Vec::new();
            }
            let mut event = make("assistant", &text);
            event.model = self.model.clone();
            return vec![event];
        }
        // developer / system prompts: record presence only, never the text.
        let mut event = make("meta", "");
        event.extra.insert("kind".into(), Value::from("system_prompt"));
        event.extra.insert("role".into(), Value::from(role));
        vec![event]
    }
}

/// `function_call_output.output` is usually a plain string; some CLI versions wrap it
/// as a JSON string or object `{ output | content, metadata }`. Unwrap the readable
/// part. A leading '{' does not guarantee JSON: the raw string IS the tool output then.
fn output_text(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => {
            let trimmed = text.trim();
            if trimmed.starts_with('{') && trimmed.contains("\"output\"") {
                if let Ok(parsed) = serde_json::from_str::<Value>(trimmed) {
                    if let Some(Value::String(output)) = parsed.get("output") {
                        return output.clone();
                    }
                }
            }
            text.clone()
        }
        Some(Value::Object(map)) => {
            if let Some(Value::String(output)) = map.get("output") {
                return output.clone();
            }
            if let Some(Value::String(content)) = map.get("content") {
                return content.clone();
            }
            serde_json::to_string(&Value::Object(map.clone())).unwrap_or_default()
        }
        Some(items @ Value::Array(_)) => serde_json::to_string(items).unwrap_or_default(),
        _ => String::new(),
    }
}
