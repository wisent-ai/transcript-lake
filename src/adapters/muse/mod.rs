//! Adapter: Muse Code (Meta's terminal agent) —
//!   `~/.local/share/muse/sessions/YYYY/MM/DD/<session id>/session.jsonl`, an
//!   event-sourced log: `{schema_version, id, stream: {kind, id}, sequence,
//!   recorded_at (microseconds), payload_type, payload}`, or a
//!   `retained_frame` whose `children[].record_json` are such records as JSON
//!   text.
//!
//! Source: the observed-format registry vshulcz/deja-vu `docs/registry/muse.md`
//! and its fixture (muse 1.3–1.4.3). The conversation is the `run` events of
//! `payload_type: runtime.session` on the session's own stream:
//!   started {prompt}                         -> user (task starts carry none)
//!   assistant_message_committed {text}       -> assistant
//!   assistant_tool_calls_committed {tool_calls: [{call_id, name, args}]}
//!                                            -> tool_call
//!   tool_result_batch_committed {results: [{tool_call_id, text}]}
//!                                            -> tool_result
//!   model_completed {usage: {input_tokens, output_tokens}, model} -> usage
//! `runtime.session.metadata {record: {workspace_root}}` names the project.
//! Records of another stream (a subagent's task) are not the person's.
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use super::common::{
    as_text, entry, event, existing_root, int_field, is_file, iso_millis, meta, subdirectories,
    text_field, tool_call, tool_result, usage,
};
use crate::types::{Adapter, Parser, ParserCtx, RawEvent, Reading, SessionEntry};

const SESSION: &str = "session.jsonl";

pub struct Muse;

impl Adapter for Muse {
    fn runtime(&self) -> &'static str {
        "muse"
    }

    fn roots(&self, home: &Path) -> Vec<PathBuf> {
        existing_root(
            home.join(".local")
                .join("share")
                .join("muse")
                .join("sessions"),
        )
    }

    fn list_sessions(&self, root: &Path) -> Vec<SessionEntry> {
        let mut sessions = Vec::new();
        for year in subdirectories(root) {
            for month in subdirectories(&year) {
                for day in subdirectories(&month) {
                    sessions.extend(
                        subdirectories(&day)
                            .iter()
                            .filter_map(|session| session_entry(session)),
                    );
                }
            }
        }
        sessions
    }

    fn entry_for(&self, path: &Path) -> Option<SessionEntry> {
        let session = path.parent()?;
        let root = session.parent()?.parent()?.parent()?.parent()?;
        let known = self
            .roots(&crate::util::home_dir())
            .iter()
            .any(|known| known.as_path() == root);
        if path.file_name()? != SESSION || !known {
            return None;
        }
        session_entry(session)
    }

    fn read(&self, ctx: ParserCtx) -> Reading {
        Reading::Lines(Box::new(MuseParser { ctx }))
    }
}

fn session_entry(session: &Path) -> Option<SessionEntry> {
    let file = session.join(SESSION);
    if !is_file(&file) {
        return None;
    }
    Some(entry(
        file,
        Some(session.file_name()?.to_string_lossy().to_string()),
        None,
    ))
}

struct MuseParser {
    ctx: ParserCtx,
}

impl MuseParser {
    fn record(&mut self, record: &Value) -> Vec<RawEvent> {
        let own = record
            .get("stream")
            .and_then(|stream| text_field(stream, "id"));
        if own.is_some() && own != self.ctx.session_id {
            return Vec::new();
        }
        let ts = int_field(record, "recorded_at").and_then(|micros| iso_millis(micros / 1000));
        let Some(payload) = record.get("payload") else {
            return Vec::new();
        };
        match text_field(record, "payload_type").as_deref() {
            Some("runtime.session.metadata") => {
                if let Some(root) = payload
                    .get("record")
                    .and_then(|inner| text_field(inner, "workspace_root"))
                {
                    self.ctx.project = Some(root);
                }
                Vec::new()
            }
            Some("runtime.session") => {
                match (text_field(payload, "kind").as_deref(), payload.get("event")) {
                    (Some("run"), Some(run)) => self.run(run, ts),
                    _ => Vec::new(),
                }
            }
            Some(other) => vec![meta(&self.ctx, ts, other, String::new(), Map::new())],
            None => Vec::new(),
        }
    }

    fn run(&self, run: &Value, ts: Option<String>) -> Vec<RawEvent> {
        let ctx = &self.ctx;
        let list = |key: &str| {
            run.get(key)
                .and_then(Value::as_array)
                .cloned()
                .into_iter()
                .flatten()
        };
        match text_field(run, "kind").as_deref() {
            Some("started") => text_field(run, "prompt")
                .map(|prompt| event(ctx, ts, "user", prompt))
                .into_iter()
                .collect(),
            Some("assistant_message_committed") => text_field(run, "text")
                .map(|text| event(ctx, ts, "assistant", text))
                .into_iter()
                .collect(),
            Some("assistant_tool_calls_committed") => list("tool_calls")
                .map(|call| {
                    let name = call.get("name").and_then(Value::as_str);
                    let id = call.get("call_id").and_then(Value::as_str);
                    tool_call(ctx, ts.clone(), name, as_text(call.get("args")), id)
                })
                .collect(),
            Some("tool_result_batch_committed") => list("results")
                .map(|result| {
                    let id = result.get("tool_call_id").and_then(Value::as_str);
                    tool_result(ctx, ts.clone(), None, as_text(result.get("text")), id, None)
                })
                .collect(),
            Some("model_completed") => {
                let spent = run.get("usage");
                let count = |key: &str| spent.and_then(|spent| int_field(spent, key));
                vec![usage(
                    ctx,
                    ts,
                    text_field(run, "model"),
                    count("input_tokens"),
                    count("output_tokens"),
                )]
            }
            _ => Vec::new(),
        }
    }
}

impl Parser for MuseParser {
    fn on_line(&mut self, line: &str) -> Vec<RawEvent> {
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            return Vec::new();
        };
        let Some(children) = record.get("children").and_then(Value::as_array) else {
            return self.record(&record);
        };
        let mut events = Vec::new();
        for child in children {
            if let Some(inner) = text_field(child, "record_json")
                .and_then(|json| serde_json::from_str::<Value>(&json).ok())
            {
                events.extend(self.record(&inner));
            }
        }
        events
    }
}
