//! Turning one recorded line into events: the parser that carries the session,
//! project and timestamp it has seen so far, holding back the records that
//! arrive before the first stamped one, and what each record type contributes.

use serde_json::{Map, Value};

use crate::types::{Parser, ParserCtx, RawEvent};

use super::{clip, epoch_iso, js_string, number, prune, text_of, PENDING_CAP};

mod message;

pub(super) struct OmpParser {
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
        let record_type = rec
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
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
        event
            .extra
            .insert("omp_type".into(), Value::from(js_string(rec.get("type"))));
        Some(vec![event])
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
