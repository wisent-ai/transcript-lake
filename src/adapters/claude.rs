//! Adapter: Claude Code transcripts — `~/.claude/projects/<encoded-cwd>/<sessionId>.jsonl`
//!
//! Frozen interface: `runtime`, `roots(home)`, `list_sessions(root)`, `parser(ctx)`.
//! Adapters emit UNMASKED text (the stream masks) and never do IO in `on_line`.
//! Contract: malformed lines are tolerated silently (no events). Verified against live
//! files on this machine: record types user | assistant | system | summary carry
//! messages; permission-mode, file-history-snapshot, attachment, ai-title, last-prompt,
//! queue-operation, progress and mode records are bookkeeping noise and are dropped.
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::types::{Adapter, Parser, ParserCtx, RawEvent, SessionEntry};

const TEXT_CAP: usize = 65536;

pub struct Claude;

impl Adapter for Claude {
    fn runtime(&self) -> &'static str {
        "claude"
    }

    fn roots(&self, home: &Path) -> Vec<PathBuf> {
        let dir = home.join(".claude").join("projects");
        if dir.exists() {
            return vec![dir];
        }
        Vec::new()
    }

    fn list_sessions(&self, root: &Path) -> Vec<SessionEntry> {
        let mut sessions = Vec::new();
        for entry in read_dirents(root) {
            if !entry.1.is_dir() {
                continue;
            }
            let project_dir = root.join(&entry.0);
            for child in read_dirents(&project_dir) {
                if !child.1.is_file() {
                    continue;
                }
                if let Some(session) = project_entry(&project_dir, &child.0) {
                    sessions.push(session);
                }
            }
        }
        sessions
    }

    fn entry_for(&self, path: &Path) -> Option<SessionEntry> {
        let name = path.file_name()?.to_string_lossy();
        let project_dir = path.parent()?;
        let home = crate::util::home_dir();
        if !self
            .roots(&home)
            .iter()
            .any(|root| project_dir.parent() == Some(root.as_path()))
        {
            return None;
        }
        if !dirent_type(project_dir)?.is_dir() || !dirent_type(path)?.is_file() {
            return None;
        }
        project_entry(project_dir, &name)
    }

    fn parser(&self, ctx: ParserCtx) -> Box<dyn Parser> {
        Box::new(ClaudeParser {
            session_id: ctx.session_id.filter(|value| !value.is_empty()),
            project: ctx.project.filter(|value| !value.is_empty()),
            last_ts: None,
        })
    }
}

/// A root or project directory may vanish between scan and read; that is not an error
/// state worth failing the stream over. Anything else (permissions, IO) was fatal in the
/// previous implementation, which could throw out of `listSessions`; the frozen Rust
/// signature cannot, so it is reported on stderr with the driver's own prefix instead.
/// Names come back in byte order, because Node's `readdirSync` sorts with strcmp and
/// the walk order decides which partition file a record lands in.
fn read_dirents(dir: &Path) -> Vec<(String, fs::FileType)> {
    let iter = match fs::read_dir(dir) {
        Ok(iter) => iter,
        Err(error) => {
            // ENOENT and ENOTDIR: the tree moved under us.
            if !matches!(error.raw_os_error(), Some(2) | Some(20)) {
                eprintln!(
                    "stream: listSessions failed under {}: {error}",
                    dir.display()
                );
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

/// The type `read_dirents` would have reported for a path the caller already knows,
/// taken without following a symlink so that an entry the scan skips is skipped here
/// too. A path that vanished between notification and read has no type at all.
fn dirent_type(path: &Path) -> Option<fs::FileType> {
    fs::symlink_metadata(path).ok().map(|meta| meta.file_type())
}

/// Directory names encode the cwd with '/' turned into '-'; the reverse mapping is best
/// effort (dashes that belonged to the real path are indistinguishable). The parser
/// prefers the per-record cwd field over this value whenever one is present.
fn decode_project_dir(name: &str) -> Option<String> {
    if !name.starts_with('-') {
        return None;
    }
    Some(name.replace('-', "/"))
}

/// The entry a scan yields for one name inside a project directory: `.jsonl` files
/// only, session id from the stem, project decoded from the directory name. Shared
/// with `entry_for` so one known path and a full scan cannot disagree about a file.
fn project_entry(project_dir: &Path, name: &str) -> Option<SessionEntry> {
    let session_id = name.strip_suffix(".jsonl")?;
    Some(SessionEntry {
        file: project_dir.join(name),
        session_id: Some(session_id.to_string()),
        project: project_dir
            .file_name()
            .and_then(|dir| decode_project_dir(&dir.to_string_lossy())),
    })
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

/// A finite JSON number field.
fn num_field(value: &Value, key: &str) -> Option<i64> {
    let raw = value.get(key).and_then(Value::as_f64)?;
    raw.is_finite().then_some(raw as i64)
}

struct ClaudeParser {
    session_id: Option<String>,
    project: Option<String>,
    last_ts: Option<String>,
}

impl Parser for ClaudeParser {
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

impl ClaudeParser {
    fn map_record(&mut self, rec: &Value) -> Vec<RawEvent> {
        if let Some(session_id) = text_field(rec, "sessionId") {
            self.session_id = Some(session_id);
        }
        if let Some(cwd) = text_field(rec, "cwd") {
            self.project = Some(cwd);
        }
        if let Some(timestamp) = text_field(rec, "timestamp") {
            self.last_ts = Some(timestamp);
        }
        let Some(ts) = self.last_ts.clone() else {
            return Vec::new();
        };
        let session_id = self.session_id.clone();
        let project = self.project.clone();
        let make = move |event_type: &str, text: &str| RawEvent {
            ts: Some(ts.clone()),
            session_id: session_id.clone(),
            project: project.clone(),
            event_type: event_type.to_string(),
            text: cap(text),
            ..RawEvent::default()
        };
        match rec.get("type").and_then(Value::as_str) {
            Some("user") => map_user(rec, &make),
            Some("assistant") => map_assistant(rec, &make),
            Some("system") => {
                let text = text_field(rec, "content")
                    .or_else(|| text_field(rec, "stopReason"))
                    .unwrap_or_default();
                let mut extra = Map::new();
                extra.insert("kind".into(), Value::from("system"));
                if let Some(subtype) = text_field(rec, "subtype") {
                    extra.insert("subtype".into(), Value::from(subtype));
                }
                let mut event = make("meta", &text);
                event.extra = extra;
                vec![event]
            }
            Some("summary") => match text_field(rec, "summary") {
                Some(summary) => {
                    let mut event = make("meta", &summary);
                    event.extra.insert("kind".into(), Value::from("summary"));
                    vec![event]
                }
                None => Vec::new(),
            },
            _ => Vec::new(),
        }
    }
}

fn map_user(rec: &Value, make: &dyn Fn(&str, &str) -> RawEvent) -> Vec<RawEvent> {
    let Some(msg) = rec.get("message").filter(|value| value.is_object()) else {
        return Vec::new();
    };
    if msg.get("role").and_then(Value::as_str) != Some("user") {
        return Vec::new();
    }
    // isMeta marks injected content (hook feedback, command wrappers), not a human turn.
    let mut event_type = "user";
    let mut flag = Map::new();
    if rec.get("isSidechain") == Some(&Value::Bool(true)) {
        flag.insert("sidechain".into(), Value::Bool(true));
    }
    if rec.get("isMeta") == Some(&Value::Bool(true)) {
        event_type = "meta";
        flag.insert("kind".into(), Value::from("injected"));
    }
    let mut events = Vec::new();
    match msg.get("content") {
        Some(Value::String(content)) => {
            if !content.trim().is_empty() {
                let mut event = make(event_type, content);
                event.extra = flag;
                events.push(event);
            }
            events
        }
        Some(Value::Array(blocks)) => {
            let mut text_parts: Vec<String> = Vec::new();
            for block in blocks {
                if !block.is_object() {
                    continue;
                }
                match block.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        if let Some(Value::String(text)) = block.get("text") {
                            text_parts.push(text.clone());
                        }
                    }
                    Some("image") => text_parts.push("[image]".to_string()),
                    Some("tool_result") => events.push(tool_result_event(block, make, &flag)),
                    _ => {}
                }
            }
            let text = text_parts.join("\n").trim().to_string();
            if !text.is_empty() {
                let mut event = make(event_type, &text);
                event.extra = flag;
                events.push(event);
            }
            events
        }
        _ => Vec::new(),
    }
}

fn tool_result_event(
    block: &Value,
    make: &dyn Fn(&str, &str) -> RawEvent,
    flag: &Map<String, Value>,
) -> RawEvent {
    let text = match block.get("content") {
        Some(Value::String(content)) => content.clone(),
        Some(Value::Array(items)) => {
            let mut parts = Vec::new();
            for item in items {
                match item.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        if let Some(Value::String(text)) = item.get("text") {
                            parts.push(text.clone());
                        }
                    }
                    Some("image") => parts.push("[image]".to_string()),
                    _ => {}
                }
            }
            parts.join("\n")
        }
        _ => String::new(),
    };
    let mut extra = flag.clone();
    if let Some(tool_use_id) = text_field(block, "tool_use_id") {
        extra.insert("tool_use_id".into(), Value::from(tool_use_id));
    }
    if block.get("is_error") == Some(&Value::Bool(true)) {
        extra.insert("is_error".into(), Value::Bool(true));
    }
    if let Some(reference) = persisted_output_path(&text) {
        extra.insert("result_file".into(), Value::from(reference));
    }
    let mut event = make("tool_result", &text);
    event.extra = extra;
    event
}

/// Large tool results are persisted beside the transcript and referenced inline as
/// `"<persisted-output>\nOutput too large (…). Full output saved to: /abs/path.txt\n…"`.
/// We record the reference path only and never follow it.
fn persisted_output_path(text: &str) -> Option<String> {
    if !text.contains("<persisted-output>") {
        return None;
    }
    let marker = "saved to: ";
    let at = text.find(marker)?;
    let start = at + marker.len();
    let stop = text[start..]
        .find('\n')
        .map_or(text.len(), |offset| start + offset);
    let reference = text[start..stop].trim();
    (!reference.is_empty()).then(|| reference.to_string())
}

fn map_assistant(rec: &Value, make: &dyn Fn(&str, &str) -> RawEvent) -> Vec<RawEvent> {
    let Some(msg) = rec.get("message").filter(|value| value.is_object()) else {
        return Vec::new();
    };
    let model = text_field(msg, "model");
    let usage = msg.get("usage").filter(|value| value.is_object());
    let mut flag = Map::new();
    if rec.get("isSidechain") == Some(&Value::Bool(true)) {
        flag.insert("sidechain".into(), Value::Bool(true));
    }
    let owned;
    let blocks: &[Value] = match msg.get("content") {
        Some(Value::Array(blocks)) => blocks,
        Some(Value::String(content)) => {
            owned = vec![serde_json::json!({ "type": "text", "text": content })];
            &owned
        }
        _ => &[],
    };
    let mut events = Vec::new();
    for block in blocks {
        if !block.is_object() {
            continue;
        }
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                if let Some(Value::String(text)) = block.get("text") {
                    if !text.trim().is_empty() {
                        let mut event = make("assistant", text);
                        event.model = model.clone();
                        event.extra = flag.clone();
                        events.push(event);
                    }
                }
            }
            Some("thinking") => {
                // Encrypted-only thinking blocks carry an empty string plus a
                // signature; skip those.
                if let Some(Value::String(thinking)) = block.get("thinking") {
                    if !thinking.trim().is_empty() {
                        let mut event = make("thinking", thinking);
                        event.model = model.clone();
                        event.extra = flag.clone();
                        events.push(event);
                    }
                }
            }
            Some("tool_use") => {
                // block.input came out of JSON parsing, so serialization cannot fail.
                let args = match block.get("input") {
                    Some(input) => serde_json::to_string(input).unwrap_or_default(),
                    None => String::new(),
                };
                let mut extra = flag.clone();
                if let Some(id) = text_field(block, "id") {
                    extra.insert("tool_use_id".into(), Value::from(id));
                }
                let mut event = make("tool_call", &args);
                event.tool_name = text_field(block, "name");
                event.model = model.clone();
                event.extra = extra;
                events.push(event);
            }
            _ => {}
        }
    }
    attach_usage(&mut events, usage, model.as_deref(), make);
    events
}

/// Usage is reported once per assistant record; attach it to the first emitted event so
/// downstream aggregation never double counts. Records whose only content is encrypted
/// thinking still surface their token spend through a small meta event.
fn attach_usage(
    events: &mut Vec<RawEvent>,
    usage: Option<&Value>,
    model: Option<&str>,
    make: &dyn Fn(&str, &str) -> RawEvent,
) {
    let Some(usage) = usage else {
        return;
    };
    let raw_input_tokens = num_field(usage, "input_tokens");
    let raw_cache_creation_tokens = num_field(usage, "cache_creation_input_tokens");
    let raw_cache_read_tokens = num_field(usage, "cache_read_input_tokens");
    let tokens_out = num_field(usage, "output_tokens");
    if raw_input_tokens.is_none()
        && raw_cache_creation_tokens.is_none()
        && raw_cache_read_tokens.is_none()
        && tokens_out.is_none()
    {
        return;
    }
    let input_tokens = raw_input_tokens.unwrap_or(0);
    let cache_creation_tokens = raw_cache_creation_tokens.unwrap_or(0);
    let cache_read_tokens = raw_cache_read_tokens.unwrap_or(0);
    let tokens_in = input_tokens + cache_creation_tokens + cache_read_tokens;
    if let Some(first) = events.first_mut() {
        first.tokens_in = Some(tokens_in);
        first.tokens_out = tokens_out;
        first
            .extra
            .insert("input_non_cached_tokens".into(), Value::from(input_tokens));
        first.extra.insert(
            "cache_creation_tokens".into(),
            Value::from(cache_creation_tokens),
        );
        first
            .extra
            .insert("cache_read_tokens".into(), Value::from(cache_read_tokens));
        return;
    }
    let mut event = make("meta", "");
    event.model = model.map(str::to_string);
    event.tokens_in = Some(tokens_in);
    event.tokens_out = tokens_out;
    event.extra.insert("kind".into(), Value::from("usage"));
    event
        .extra
        .insert("input_non_cached_tokens".into(), Value::from(input_tokens));
    event.extra.insert(
        "cache_creation_tokens".into(),
        Value::from(cache_creation_tokens),
    );
    event
        .extra
        .insert("cache_read_tokens".into(), Value::from(cache_read_tokens));
    events.push(event);
}
