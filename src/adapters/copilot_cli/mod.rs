//! Adapter: GitHub Copilot CLI —
//!   `~/.copilot/session-state/<session id>/events.jsonl`, appended one event
//!   per line: `{type, data, id, parentId, timestamp}` (ISO 8601).
//!
//! Source: the observed-format registry vshulcz/deja-vu
//! `docs/registry/copilot.md` and its fixture (Copilot CLI 1.0.71–1.0.92):
//!   session.start {sessionId, context: {cwd}}  -> meta; names the session and
//!                                                 the project for what follows
//!   session.model_change {newModel}            -> meta, the model from then on
//!   user.message {content}                     -> user
//!   assistant.message {content}                -> assistant
//!   tool.execution_start {toolCallId?, toolName, arguments}   -> tool_call
//!   tool.execution_complete {toolCallId?, toolName?, success, result: {content}}
//!                                              -> tool_result (its `success`
//!                                                 is true on failed runs too, so
//!                                                 it is not read as the outcome)
//!   session.compaction_complete {summaryContent} -> meta carrying the summary
//!   system.message and every other type        -> meta named by its type
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use super::common::{
    as_text, entry, event, existing_root, is_file, meta, subdirectories, text_field, tool_call,
    tool_result,
};
use crate::types::{Adapter, Parser, ParserCtx, RawEvent, Reading, SessionEntry};

const EVENTS: &str = "events.jsonl";

pub struct CopilotCli;

impl Adapter for CopilotCli {
    fn runtime(&self) -> &'static str {
        "copilot"
    }

    fn roots(&self, home: &Path) -> Vec<PathBuf> {
        existing_root(home.join(".copilot").join("session-state"))
    }

    fn list_sessions(&self, root: &Path) -> Vec<SessionEntry> {
        subdirectories(root)
            .iter()
            .filter_map(|session| session_entry(session))
            .collect()
    }

    fn entry_for(&self, path: &Path) -> Option<SessionEntry> {
        let session = path.parent()?;
        if path.file_name()? != EVENTS
            || !self
                .roots(&crate::util::home_dir())
                .iter()
                .any(|root| session.parent() == Some(root.as_path()))
        {
            return None;
        }
        session_entry(session)
    }

    fn read(&self, ctx: ParserCtx) -> Reading {
        Reading::Lines(Box::new(CopilotParser { ctx, model: None }))
    }
}

fn session_entry(session: &Path) -> Option<SessionEntry> {
    let events = session.join(EVENTS);
    if !is_file(&events) {
        return None;
    }
    Some(entry(
        events,
        Some(session.file_name()?.to_string_lossy().to_string()),
        None,
    ))
}

struct CopilotParser {
    ctx: ParserCtx,
    model: Option<String>,
}

impl Parser for CopilotParser {
    fn on_line(&mut self, line: &str) -> Vec<RawEvent> {
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            return Vec::new();
        };
        let (Some(kind), Some(data)) = (
            record.get("type").and_then(Value::as_str),
            record.get("data"),
        ) else {
            return Vec::new();
        };
        let ts = text_field(&record, "timestamp");
        let id = data.get("toolCallId").and_then(Value::as_str);
        let name = data.get("toolName").and_then(Value::as_str);
        match kind {
            "session.start" => {
                if let Some(session) = text_field(data, "sessionId") {
                    self.ctx.session_id = Some(session);
                }
                if let Some(cwd) = data
                    .get("context")
                    .and_then(|context| text_field(context, "cwd"))
                {
                    self.ctx.project = Some(cwd);
                }
                vec![meta(&self.ctx, ts, kind, String::new(), Map::new())]
            }
            "session.model_change" => {
                self.model = text_field(data, "newModel");
                vec![meta(
                    &self.ctx,
                    ts,
                    kind,
                    as_text(data.get("newModel")),
                    Map::new(),
                )]
            }
            "user.message" => text_field(data, "content")
                .map(|text| event(&self.ctx, ts, "user", text))
                .into_iter()
                .collect(),
            "assistant.message" => text_field(data, "content")
                .map(|text| {
                    let mut reply = event(&self.ctx, ts, "assistant", text);
                    reply.model = self.model.clone();
                    reply
                })
                .into_iter()
                .collect(),
            "tool.execution_start" => vec![tool_call(
                &self.ctx,
                ts,
                name,
                as_text(data.get("arguments")),
                id,
            )],
            "tool.execution_complete" => {
                let output = data.get("result").and_then(|result| result.get("content"));
                vec![tool_result(&self.ctx, ts, name, as_text(output), id, None)]
            }
            "session.compaction_complete" => {
                vec![meta(
                    &self.ctx,
                    ts,
                    kind,
                    as_text(data.get("summaryContent")),
                    Map::new(),
                )]
            }
            other => vec![meta(&self.ctx, ts, other, String::new(), Map::new())],
        }
    }
}
