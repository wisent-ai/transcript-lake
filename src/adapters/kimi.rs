//! Adapter: Kimi Code CLI wire transcripts —
//!   `~/.kimi-code/sessions/wd_*/session_*/agents/main/wire.jsonl`
//!
//! Frozen interface: `runtime`, `roots(home)`, `list_sessions(root)`, `parser(ctx)`.
//! Adapters emit UNMASKED text (the ingest driver masks) and never do IO in `on_line`.
//! Shapes verified against the largest live wire files on this machine:
//!   metadata {protocol_version, app_version, created_at(epoch ms)}
//!   config.update {profileName, systemPrompt}          -> meta only, prompt dropped
//!   context.append_message {message:{role, content:[{type:'text', text}],
//!     origin:{kind}}, time} — origin.kind 'user' is the human turn; 'injection'
//!     and 'background_task' are synthetic context and become meta events.
//!   turn.prompt / turn.steer duplicate append_message one-to-one -> skipped.
//!   context.append_loop_event {event:{type}, time} carries the assistant loop:
//!     content.part {part:{type:'text'|'think'}}         (each part arrives complete)
//!     tool.call    {toolCallId, name, args, description}
//!     tool.result  {toolCallId, result:{output, isError?}}
//!     step.begin / step.end                             -> dropped (usage.record wins)
//!   usage.record {model, usage:{input*, output}, usageScope:'turn'|'session'} —
//!     'turn' records mirror step.end usage exactly (per-step deltas, summable);
//!     'session' records are cumulative snapshots and only refresh the model.
//!   context.apply_compaction {summary} and turn.cancel  -> small meta events.
//! Wire records carry no session id or cwd; the session id is the `session_*` dirname
//! and `~/.kimi-code/session_index.jsonl` maps sessionId -> workDir (the only place the
//! absolute project path exists, so `list_sessions` performs that one small read).
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::types::{Adapter, Parser, ParserCtx, RawEvent, SessionEntry};

const TEXT_CAP: usize = 65536;

pub struct Kimi;

impl Adapter for Kimi {
    fn runtime(&self) -> &'static str {
        "kimi"
    }

    fn roots(&self, home: &Path) -> Vec<PathBuf> {
        let dir = home.join(".kimi-code").join("sessions");
        if dir.exists() {
            return vec![dir];
        }
        Vec::new()
    }

    fn list_sessions(&self, root: &Path) -> Vec<SessionEntry> {
        let index = read_work_dir_index(root);
        let mut sessions = Vec::new();
        for (name, file_type) in read_dirents(root) {
            if !file_type.is_dir() || !name.starts_with("wd_") {
                continue;
            }
            let work_dir = root.join(&name);
            for (entry, entry_type) in read_dirents(&work_dir) {
                if !entry_type.is_dir() {
                    continue;
                }
                if let Some(session) = session_entry(&work_dir, &entry, &index) {
                    sessions.push(session);
                }
            }
        }
        sessions
    }

    fn entry_for(&self, path: &Path) -> Option<SessionEntry> {
        // The wire transcript is the only file a scan ever offers, and it sits at a
        // fixed depth: `<root>/wd_*/session_*/agents/main/wire.jsonl`.
        if path.file_name()?.to_string_lossy() != "wire.jsonl" {
            return None;
        }
        let main = path.parent()?;
        let agents = main.parent()?;
        if main.file_name()?.to_string_lossy() != "main"
            || agents.file_name()?.to_string_lossy() != "agents"
        {
            return None;
        }
        let session_dir = agents.parent()?;
        let work_dir = session_dir.parent()?;
        let home = crate::util::home_dir();
        let root =
            self.roots(&home).into_iter().find(|root| work_dir.parent() == Some(root.as_path()))?;
        if !work_dir.file_name()?.to_string_lossy().starts_with("wd_")
            || !dirent_type(work_dir)?.is_dir()
            || !dirent_type(session_dir)?.is_dir()
        {
            return None;
        }
        let name = session_dir.file_name()?.to_string_lossy();
        // The work-dir index is the only place the absolute project path exists, so
        // resolving one file pays the same small read the scan pays once per root.
        session_entry(work_dir, &name, &read_work_dir_index(&root))
    }

    fn parser(&self, ctx: ParserCtx) -> Box<dyn Parser> {
        Box::new(KimiParser {
            session_id: ctx.session_id.filter(|value| !value.is_empty()),
            project: ctx.project.filter(|value| !value.is_empty()),
            last_ts: None,
            last_model: None,
            tool_names: HashMap::new(),
        })
    }
}

/// A root or session directory may vanish between scan and read; that is not an error
/// state worth failing ingestion over. Anything else (permissions, IO) was fatal in the
/// previous implementation, which could throw out of `listSessions`; the frozen Rust
/// signature cannot, so it is reported on stderr with the driver's own prefix instead.
/// Names come back in byte order, because Node's `readdirSync` sorts with strcmp.
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

/// The type `read_dirents` would have reported for a path the caller already knows,
/// taken without following a symlink so that an entry the scan skips is skipped here
/// too. A path that vanished between notification and read has no type at all.
fn dirent_type(path: &Path) -> Option<fs::FileType> {
    fs::symlink_metadata(path).ok().map(|meta| meta.file_type())
}

/// The entry a scan yields for one directory inside a work directory: `session_*` only,
/// and only once it holds a wire transcript, with the directory name as the session id
/// and the project the index records for it. Shared with `entry_for` so one known path
/// and a full scan cannot disagree about a file.
fn session_entry(
    work_dir: &Path,
    name: &str,
    index: &HashMap<String, String>,
) -> Option<SessionEntry> {
    if !name.starts_with("session_") {
        return None;
    }
    let file = work_dir.join(name).join("agents").join("main").join("wire.jsonl");
    if !file.exists() {
        return None;
    }
    Some(SessionEntry {
        file,
        session_id: Some(name.to_string()),
        project: index.get(name).cloned(),
    })
}

fn read_work_dir_index(root: &Path) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let Some(parent) = root.parent() else {
        return map;
    };
    let Ok(text) = fs::read_to_string(parent.join("session_index.jsonl")) else {
        return map;
    };
    for line in text.split('\n') {
        if line.trim().is_empty() {
            continue;
        }
        // A torn index line only costs a project attribution, never the session.
        let Ok(rec) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let (Some(Value::String(session_id)), Some(Value::String(work_dir))) =
            (rec.get("sessionId"), rec.get("workDir"))
        {
            map.insert(session_id.clone(), work_dir.clone());
        }
    }
    map
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

/// Wire times are epoch milliseconds; an out-of-range value must not kill the line.
fn iso_from(value: Option<&Value>) -> Option<String> {
    let millis = value?.as_f64().filter(|raw| raw.is_finite())?;
    let stamp = chrono::DateTime::from_timestamp_millis(millis as i64)?;
    Some(stamp.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
}

struct KimiParser {
    session_id: Option<String>,
    project: Option<String>,
    last_ts: Option<String>,
    last_model: Option<String>,
    tool_names: HashMap<String, String>,
}

impl Parser for KimiParser {
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

impl KimiParser {
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

    fn map_record(&mut self, rec: &Value) -> Vec<RawEvent> {
        let ts = iso_from(rec.get("time"))
            .or_else(|| iso_from(rec.get("created_at")))
            .or_else(|| self.last_ts.clone());
        let Some(ts) = ts.filter(|value| !value.is_empty()) else {
            return Vec::new();
        };
        self.last_ts = Some(ts.clone());
        let record_type = rec.get("type").and_then(Value::as_str).unwrap_or_default();
        if record_type == "context.append_loop_event" {
            return self.map_loop_event(rec.get("event"), &ts);
        }
        if record_type == "context.append_message" {
            return map_message(rec.get("message"), &self.maker(&ts));
        }
        if record_type == "usage.record" {
            return self.map_usage(rec, &ts);
        }
        let make = self.maker(&ts);
        if record_type == "metadata" {
            let mut extra = Map::new();
            extra.insert("kind".into(), Value::from("metadata"));
            if let Some(protocol) = text_field(rec, "protocol_version") {
                extra.insert("protocol".into(), Value::from(protocol));
            }
            if let Some(app) = text_field(rec, "app_version") {
                extra.insert("app".into(), Value::from(app));
            }
            let mut event = make("meta", "");
            event.extra = extra;
            return vec![event];
        }
        if record_type == "config.update" {
            // The systemPrompt payload is deliberately dropped: meta at most, no text.
            let mut extra = Map::new();
            extra.insert("kind".into(), Value::from("config"));
            if let Some(profile) = text_field(rec, "profileName") {
                extra.insert("profile".into(), Value::from(profile));
            }
            let mut event = make("meta", "");
            event.extra = extra;
            return vec![event];
        }
        if record_type == "context.apply_compaction" {
            if let Some(summary) = text_field(rec, "summary") {
                let mut event = make("meta", &summary);
                event.extra.insert("kind".into(), Value::from("compaction"));
                return vec![event];
            }
            return Vec::new();
        }
        if record_type == "turn.cancel" {
            let mut event = make("meta", "");
            event.extra.insert("kind".into(), Value::from("turn.cancel"));
            return vec![event];
        }
        // turn.prompt / turn.steer echo append_message; tools.*, permission.*, *_mode.*,
        // and compaction bookkeeping records carry no conversational content.
        Vec::new()
    }

    fn map_loop_event(&mut self, event: Option<&Value>, ts: &str) -> Vec<RawEvent> {
        let Some(ev) = event.filter(|value| value.is_object()) else {
            return Vec::new();
        };
        let make = self.maker(ts);
        match ev.get("type").and_then(Value::as_str) {
            Some("content.part") => {
                let Some(part) = ev.get("part").filter(|value| value.is_object()) else {
                    return Vec::new();
                };
                match part.get("type").and_then(Value::as_str) {
                    Some("text") => match text_field(part, "text") {
                        Some(text) => {
                            let mut out = make("assistant", &text);
                            out.model = self.last_model.clone();
                            vec![out]
                        }
                        None => Vec::new(),
                    },
                    Some("think") => match text_field(part, "think") {
                        Some(text) => {
                            let mut out = make("thinking", &text);
                            out.model = self.last_model.clone();
                            vec![out]
                        }
                        None => Vec::new(),
                    },
                    _ => Vec::new(),
                }
            }
            Some("tool.call") => {
                let call_id = text_field(ev, "toolCallId").or_else(|| text_field(ev, "uuid"));
                let name = text_field(ev, "name");
                if let (Some(call_id), Some(name)) = (&call_id, &name) {
                    self.tool_names.insert(call_id.clone(), name.clone());
                }
                // Mirror the claude adapter: the argument JSON is the searchable text.
                let mut text = match ev.get("args") {
                    Some(args) => serde_json::to_string(args).unwrap_or_default(),
                    None => String::new(),
                };
                if text.is_empty() {
                    if let Some(description) = text_field(ev, "description") {
                        text = description;
                    }
                }
                let mut extra = Map::new();
                if let Some(call_id) = &call_id {
                    extra.insert("tool_use_id".into(), Value::from(call_id.clone()));
                }
                let mut out = make("tool_call", &text);
                out.tool_name = name;
                out.model = self.last_model.clone();
                out.extra = extra;
                vec![out]
            }
            Some("tool.result") => {
                let call_id = text_field(ev, "toolCallId").or_else(|| text_field(ev, "parentUuid"));
                let result = ev.get("result").filter(|value| value.is_object());
                let text = match result.and_then(|result| result.get("output")) {
                    Some(Value::String(output)) => output.clone(),
                    _ => match ev.get("result") {
                        Some(Value::String(output)) => output.clone(),
                        _ => String::new(),
                    },
                };
                let tool_name =
                    call_id.as_ref().and_then(|call_id| self.tool_names.get(call_id).cloned());
                let mut extra = Map::new();
                if let Some(call_id) = &call_id {
                    extra.insert("tool_use_id".into(), Value::from(call_id.clone()));
                }
                let is_error = result
                    .and_then(|result| result.get("isError"))
                    .is_some_and(is_truthy);
                if is_error {
                    extra.insert("is_error".into(), Value::Bool(true));
                }
                let mut out = make("tool_result", &text);
                out.tool_name = tool_name;
                out.extra = extra;
                vec![out]
            }
            // step.begin/step.end carry loop bookkeeping only; usage.record owns tokens.
            _ => Vec::new(),
        }
    }

    /// 'turn'-scoped usage records are per-step deltas (verified identical to step.end
    /// usage), so summing them downstream yields honest session totals. 'session'-scoped
    /// snapshots would double count and only refresh the current model name.
    fn map_usage(&mut self, rec: &Value, ts: &str) -> Vec<RawEvent> {
        if let Some(model) = text_field(rec, "model") {
            self.last_model = Some(model);
        }
        if rec.get("usageScope").and_then(Value::as_str) != Some("turn") {
            return Vec::new();
        }
        let empty = Value::Object(Map::new());
        let usage = rec.get("usage").filter(|value| value.is_object()).unwrap_or(&empty);
        let input_other = num_or_zero(usage, "inputOther");
        let cache_read = num_or_zero(usage, "inputCacheRead");
        let cache_creation = num_or_zero(usage, "inputCacheCreation");
        let tokens_in = input_other + cache_read + cache_creation;
        let tokens_out = num_or_zero(usage, "output");
        if tokens_in + tokens_out == 0 {
            return Vec::new();
        }
        let mut event = self.maker(ts)("meta", "");
        event.model = self.last_model.clone();
        event.tokens_in = Some(tokens_in);
        event.tokens_out = Some(tokens_out);
        event.extra.insert("kind".into(), Value::from("usage"));
        event.extra.insert("input_non_cached_tokens".into(), Value::from(input_other));
        event.extra.insert("cache_creation_tokens".into(), Value::from(cache_creation));
        event.extra.insert("cache_read_tokens".into(), Value::from(cache_read));
        vec![event]
    }
}

/// JS truthiness for the `result.isError` guard, which was a bare `if (result.isError)`.
fn is_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64().is_some_and(|raw| raw != 0.0 && !raw.is_nan()),
        Value::String(text) => !text.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

fn map_message(msg: Option<&Value>, make: &dyn Fn(&str, &str) -> RawEvent) -> Vec<RawEvent> {
    let Some(msg) = msg.filter(|value| value.is_object()) else {
        return Vec::new();
    };
    let Some(role) = text_field(msg, "role") else {
        return Vec::new();
    };
    let mut parts = Vec::new();
    match msg.get("content") {
        Some(Value::Array(blocks)) => {
            for block in blocks {
                if block.get("type").and_then(Value::as_str) == Some("text") {
                    if let Some(Value::String(text)) = block.get("text") {
                        parts.push(text.clone());
                    }
                }
            }
        }
        Some(Value::String(content)) => parts.push(content.clone()),
        _ => {}
    }
    let text = parts.join("\n").trim().to_string();
    if text.is_empty() {
        return Vec::new();
    }
    let origin = msg
        .get("origin")
        .filter(|value| value.is_object())
        .and_then(|origin| text_field(origin, "kind"));
    if role == "user" {
        if let Some(origin) = origin.filter(|origin| origin != "user") {
            // Injections and background-task notifications are synthetic context,
            // not turns.
            let mut event = make("meta", &text);
            event.extra.insert("kind".into(), Value::from("injected"));
            event.extra.insert("origin".into(), Value::from(origin));
            return vec![event];
        }
        return vec![make("user", &text)];
    }
    if role == "assistant" {
        return vec![make("assistant", &text)];
    }
    Vec::new()
}
