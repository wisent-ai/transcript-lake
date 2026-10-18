//! The transcript itself: the parser that carries the session, project and
//! timestamp it has seen so far, and what each record contributes - including
//! a tool call whose result arrives on a later line.

use std::path::PathBuf;

use serde_json::{Map, Value};

use crate::types::{Parser, ParserCtx, RawEvent};

use crate::adapters::common::{appended_record, iso_epoch_millis, js_string, prune, text_joined};

pub(super) struct TranscriptParser {
    file: PathBuf,
    project: Option<String>,
    session_id: Option<String>,
    last_ts: Option<String>,
    pending: Vec<RawEvent>,
    ready: bool,
}

impl TranscriptParser {
    /// Records before the first stamped one are held back, because their
    /// timestamp and working directory only arrive later in the file.
    pub(super) fn new(ctx: ParserCtx) -> Self {
        Self {
            project: ctx.project.clone(),
            session_id: ctx.session_id.clone(),
            file: ctx.file,
            last_ts: None,
            pending: Vec::new(),
            ready: false,
        }
    }
}

impl Parser for TranscriptParser {
    fn on_line(&mut self, line: &str) -> Vec<RawEvent> {
        let Some(rec) = appended_record(&self.file, line) else {
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
        if self.last_ts.is_some() {
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
    fn make(&self, ts: Option<&String>, event_type: &str, text: Option<&str>) -> RawEvent {
        RawEvent {
            ts: ts.cloned(),
            session_id: self.session_id.clone(),
            project: self.project.clone(),
            event_type: event_type.to_string(),
            text: text.map(str::to_string),
            ..RawEvent::default()
        }
    }

    /// `None` means the record's epoch timestamp is unrepresentable, which threw
    /// inside `new Date(ms).toISOString()` and dropped the whole line.
    fn stamp(&mut self, rec: &Value) -> Option<Option<String>> {
        let ts = match rec.get("timestamp") {
            Some(Value::String(text)) => Some(text.clone()),
            Some(Value::Number(number)) => Some(iso_epoch_millis(number.as_f64()?)?),
            _ => None,
        };
        if let Some(ts) = &ts {
            self.last_ts = Some(ts.clone());
        }
        Some(ts.or_else(|| self.last_ts.clone()))
    }

    fn handle(&mut self, rec: &Value) -> Option<Vec<RawEvent>> {
        let ts = self.stamp(rec)?;
        // A record whose type is not text is none of the kinds below; it
        // becomes the unknown meta event at the end.
        match rec.get("type").and_then(Value::as_str) {
            Some("session_start") => {
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
                    Some(Value::String(title)) if !title.is_empty() => Some(title.as_str()),
                    _ => rec.get("title").and_then(Value::as_str),
                };
                let mut extra = Map::new();
                extra.insert("kind".into(), Value::from("session_start"));
                prune(&mut extra, "version", rec.get("version"));
                let mut event = self.make(ts.as_ref(), "meta", title);
                event.extra = extra;
                Some(vec![event])
            }
            Some("message") => Some(self.message_events(rec, ts.as_ref())),
            Some("todo_state") => {
                let mut event = self.make(ts.as_ref(), "meta", None);
                event.extra.insert("kind".into(), Value::from("todo_state"));
                // A todo state that records no list says nothing about its count.
                if let Some(todos) = rec
                    .get("todos")
                    .and_then(|box_value| box_value.get("todos"))
                    .and_then(Value::as_array)
                {
                    event
                        .extra
                        .insert("todo_count".into(), Value::from(todos.len()));
                }
                Some(vec![event])
            }
            Some("compaction_state") => {
                let summary = rec.get("summaryText").and_then(Value::as_str);
                let mut event = self.make(ts.as_ref(), "meta", summary);
                event
                    .extra
                    .insert("kind".into(), Value::from("compaction_state"));
                Some(vec![event])
            }
            _ => {
                let mut event = self.make(ts.as_ref(), "meta", None);
                event.extra.insert("kind".into(), Value::from("unknown"));
                event
                    .extra
                    .insert("droid_type".into(), Value::from(js_string(rec.get("type"))));
                Some(vec![event])
            }
        }
    }

    fn message_events(&self, rec: &Value, ts: Option<&String>) -> Vec<RawEvent> {
        let Some(msg) = rec.get("message").filter(|value| value.is_object()) else {
            let mut event = self.make(ts, "meta", None);
            event.extra.insert("kind".into(), Value::from("message"));
            return vec![event];
        };
        let role = match msg.get("role") {
            Some(Value::String(role)) => role.clone(),
            _ => "unknown".to_string(),
        };
        // Factory marks provider-generated notices with visibility, including
        // notices displayed in the UI. They are not typed operator turns.
        let visibility = msg.get("visibility");
        let text_type = match role.as_str() {
            "user" if visibility.is_none() => "user",
            "assistant" => "assistant",
            _ => "meta",
        };
        let mut events: Vec<RawEvent> = Vec::new();
        let mut buffer: Vec<String> = Vec::new();
        let owned;
        let blocks: &[Value] = match msg.get("content") {
            Some(Value::Array(blocks)) => blocks,
            other => {
                owned = vec![serde_json::json!({ "type": "text", "text": text_joined(other) })];
                &owned
            }
        };
        for block in blocks {
            if !block.is_object() {
                continue;
            }
            match block.get("type").and_then(Value::as_str) {
                Some("text") => {
                    // A text block without text adds no words.
                    if let Some(Value::String(text)) = block.get("text") {
                        buffer.push(text.clone());
                    }
                }
                Some("thinking") => {
                    self.flush_text(&mut events, &mut buffer, ts, text_type, &role, visibility);
                    let thinking = block.get("thinking").and_then(Value::as_str);
                    events.push(self.make(ts, "thinking", thinking));
                }
                Some("tool_use") => {
                    self.flush_text(&mut events, &mut buffer, ts, text_type, &role, visibility);
                    // The same shape as the omp adapter's toolCall: the input
                    // is the row's text, so a reader sees what was run.
                    let args = block.get("input").map(Value::to_string);
                    let mut event = self.make(ts, "tool_call", args.as_deref());
                    event.tool_name = match block.get("name") {
                        Some(Value::String(name)) => Some(name.clone()),
                        _ => None,
                    };
                    prune(&mut event.extra, "call_id", block.get("id"));
                    events.push(event);
                }
                Some("tool_result") => {
                    self.flush_text(&mut events, &mut buffer, ts, text_type, &role, visibility);
                    let output = text_joined(block.get("content"));
                    let mut event = self.make(ts, "tool_result", Some(&output));
                    prune(&mut event.extra, "call_id", block.get("tool_use_id"));
                    events.push(event);
                }
                _ => {
                    self.flush_text(&mut events, &mut buffer, ts, text_type, &role, visibility);
                    let mut event = self.make(ts, "meta", None);
                    event.extra.insert("kind".into(), Value::from("block"));
                    event.extra.insert(
                        "droid_block".into(),
                        Value::from(js_string(block.get("type"))),
                    );
                    events.push(event);
                }
            }
        }
        self.flush_text(&mut events, &mut buffer, ts, text_type, &role, visibility);
        events
    }

    fn flush_text(
        &self,
        events: &mut Vec<RawEvent>,
        buffer: &mut Vec<String>,
        ts: Option<&String>,
        text_type: &str,
        role: &str,
        visibility: Option<&Value>,
    ) {
        if buffer.is_empty() {
            return;
        }
        let mut event = self.make(ts, text_type, Some(&buffer.join("\n")));
        event.extra.insert("role".into(), Value::from(role));
        if text_type == "meta" {
            event.extra.insert("kind".into(), Value::from("message"));
            prune(&mut event.extra, "visibility", visibility);
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
