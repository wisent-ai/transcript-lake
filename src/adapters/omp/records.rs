//! Turning one recorded line into events: the parser that carries the session,
//! project and timestamp it has seen so far, holding back the records that
//! arrive before the first stamped one, and what each record type contributes.

use std::path::PathBuf;

use serde_json::{Map, Value};

use crate::types::{Parser, ParserCtx, RawEvent};

use crate::adapters::common::{appended_record, iso_epoch_millis, js_string, prune, text_joined};

mod message;

pub(super) struct OmpParser {
    file: PathBuf,
    project: Option<String>,
    session_id: Option<String>,
    last_ts: Option<String>,
    model: Option<String>,
    pending: Vec<RawEvent>,
    ready: bool,
}

impl OmpParser {
    /// Records before the first stamped one are held back, because their
    /// timestamp and working directory only arrive later in the file.
    pub(super) fn new(ctx: ParserCtx) -> Self {
        Self {
            file: ctx.file,
            project: ctx.project,
            session_id: ctx.session_id,
            last_ts: None,
            model: None,
            pending: Vec::new(),
            ready: false,
        }
    }
}

impl Parser for OmpParser {
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
        if rec.get("type").and_then(Value::as_str) == Some("session") {
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

    /// A metadata event of record type `kind` with the record's `text`.
    fn noted(&self, ts: Option<&String>, kind: &str, text: Option<&str>) -> RawEvent {
        let mut event = self.make(ts, "meta", text);
        event.extra.insert("kind".into(), Value::from(kind));
        event
    }

    /// `None` means the record's epoch timestamp is unrepresentable, which threw
    /// inside `new Date(ms).toISOString()` and dropped the whole line.
    fn stamp(&mut self, rec: &Value) -> Option<Option<String>> {
        let ts = match rec.get("timestamp") {
            Some(Value::String(text)) => Some(text.clone()),
            Some(Value::Number(number)) => Some(iso_epoch_millis(number.as_f64()?)?),
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
        // A record whose type is not text is none of the kinds below; it
        // becomes the unknown meta event at the end, carrying what it held.
        let at = ts.as_ref();
        match rec.get("type").and_then(Value::as_str) {
            Some("session") => {
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
                let mut event = self.noted(at, "session", None);
                prune(&mut event.extra, "version", rec.get("version"));
                Some(vec![event])
            }
            Some("message") => Some(self.message_events(rec, at)),
            Some(kind @ "title") => {
                let title = rec.get("title").and_then(Value::as_str);
                Some(vec![self.noted(at, kind, title)])
            }
            Some(kind @ "title_change") => {
                let title = rec.get("title").and_then(Value::as_str);
                Some(vec![self.noted(at, kind, title)])
            }
            Some("model_change") => {
                if let Some(Value::String(model)) = rec.get("model") {
                    self.model = Some(model.clone());
                }
                let mut event = self.noted(at, "model_change", None);
                event.model = self.model.clone();
                Some(vec![event])
            }
            Some("thinking_level_change") => {
                let mut event = self.noted(at, "thinking_level_change", None);
                prune(&mut event.extra, "level", rec.get("thinkingLevel"));
                Some(vec![event])
            }
            Some("compaction") => {
                let summary = rec.get("summary").and_then(Value::as_str);
                let mut event = self.noted(at, "compaction", summary);
                prune(&mut event.extra, "tokens_before", rec.get("tokensBefore"));
                prune(
                    &mut event.extra,
                    "first_kept_entry",
                    rec.get("firstKeptEntryId"),
                );
                Some(vec![event])
            }
            Some(kind @ "custom_message") => Some(vec![self.custom(at, kind, rec)]),
            Some(kind @ "custom") => Some(vec![self.custom(at, kind, rec)]),
            _ => {
                let mut event = self.noted(at, "unknown", None);
                event
                    .extra
                    .insert("omp_type".into(), Value::from(js_string(rec.get("type"))));
                Some(vec![event])
            }
        }
    }

    /// A custom record: its content as text and its custom type.
    fn custom(&self, ts: Option<&String>, kind: &str, rec: &Value) -> RawEvent {
        let content = text_joined(rec.get("content"));
        let mut event = self.noted(ts, kind, Some(&content));
        prune(&mut event.extra, "custom_type", rec.get("customType"));
        event
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
        let mut event = self.make(ts, text_type, Some(&buffer.join("\n")));
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
