//! Adapter: Jeden, Wisent's own harness —
//!   `~/.jeden/sessions/<session-id>/transcript.jsonl`, appended one event per
//!   line, beside `state.json` (`{version, id, cwd, startedAt, ...}`), the only
//!   place the session's working directory is recorded.
//!
//! Record shapes, read from real transcripts on this machine and from Oko's
//! Jeden decoder (`oko/rust/src/decode/harness.rs`): schema 2 wraps each event
//! as `{eventId, sessionId, parentId, sequence, timestamp: "<unix seconds>",
//! schemaVersion, payload: {type, data}}`; older ledgers are flat
//! `{type, data, ts}`.
//!   user {task | text, modelOnly?}       -> user (a model-only call a program
//!                                           makes is metadata, not the operator)
//!   final {step, text}                   -> assistant
//!   tool_call {step, tool, input}        -> tool_call
//!   tool_result {step, tool, result}     -> tool_result (`result.ok == false`
//!                                           is a failure)
//!   run_error {operation, message}       -> meta carrying the message
//!   agent_state {purpose}                -> meta `delegated_session`: Jeden
//!                                           opened it for another session
//!   action / assistant_raw               -> skipped: they repeat the model's
//!                                           answer that final / tool_call carry
//!   anything else (completion_state, task_contract, context_snapshot,
//!   lineage, ...)                        -> meta named by its type
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use super::common::{
    appended_record, as_text, entry, event, existing_root, is_file, iso_seconds, meta,
    sidecar_json, subdirectories, text_field, tool_call, tool_result,
};
use crate::types::{Adapter, Parser, ParserCtx, RawEvent, Reading, SessionEntry};

const TRANSCRIPT: &str = "transcript.jsonl";
const STATE: &str = "state.json";

pub struct Jeden;

impl Adapter for Jeden {
    fn runtime(&self) -> &'static str {
        "jeden"
    }

    fn roots(&self, home: &Path) -> Vec<PathBuf> {
        existing_root(home.join(".jeden").join("sessions"))
    }

    fn list_sessions(&self, root: &Path) -> Vec<SessionEntry> {
        subdirectories(root)
            .into_iter()
            .filter_map(|session| session_entry(&session))
            .collect()
    }

    fn entry_for(&self, path: &Path) -> Option<SessionEntry> {
        if path.file_name()? != TRANSCRIPT {
            return None;
        }
        let session = path.parent()?;
        if !self
            .roots(&crate::util::home_dir())
            .iter()
            .any(|root| session.parent() == Some(root.as_path()))
        {
            return None;
        }
        session_entry(session)
    }

    fn read(&self, ctx: ParserCtx) -> Reading {
        Reading::Lines(Box::new(JedenParser { ctx }))
    }
}

/// The transcript of one session directory, with the session id its
/// directory is named by and the working directory its state records.
fn session_entry(session: &Path) -> Option<SessionEntry> {
    let transcript = session.join(TRANSCRIPT);
    if !is_file(&transcript) {
        return None;
    }
    let project = sidecar_json(&session.join(STATE)).and_then(|state| text_field(&state, "cwd"));
    let id = session.file_name()?.to_string_lossy().to_string();
    Some(entry(transcript, Some(id), project))
}

struct JedenParser {
    ctx: ParserCtx,
}

impl Parser for JedenParser {
    fn on_line(&mut self, line: &str) -> Vec<RawEvent> {
        let Some(record) = appended_record(&self.ctx.file, line) else {
            return Vec::new();
        };
        // Schema 2 wraps the event in `payload`; an older ledger is the event.
        let payload = match record.get("payload") {
            Some(payload) => payload,
            None => &record,
        };
        let (Some(kind), Some(data)) = (
            payload.get("type").and_then(Value::as_str),
            payload.get("data"),
        ) else {
            return Vec::new();
        };
        // Jeden stamps a line in seconds since the epoch, as a number or as
        // its text; text that is not a number leaves the line without a time
        // and is said, rather than read as no stamp at all.
        let ts = match record.get("timestamp").or_else(|| record.get("ts")) {
            Some(Value::String(text)) => {
                match text.parse::<f64>() {
                    Ok(seconds) => iso_seconds(seconds),
                    Err(error) => {
                        eprintln!("jeden sessions: the stamp {text:?} is not a number of seconds: {error}");
                        None
                    }
                }
            }
            Some(stamp) => stamp.as_f64().and_then(iso_seconds),
            None => None,
        };
        let mut events = self.events(kind, data, &record, ts);
        if let Some(session) = text_field(&record, "sessionId") {
            for event in &mut events {
                event.session_id = Some(session.clone());
            }
        }
        events
    }
}

impl JedenParser {
    fn events(
        &self,
        kind: &str,
        data: &Value,
        record: &Value,
        ts: Option<String>,
    ) -> Vec<RawEvent> {
        let ctx = &self.ctx;
        match kind {
            "user" => match text_field(data, "task").or_else(|| text_field(data, "text")) {
                Some(task) if data.get("modelOnly").and_then(Value::as_bool) == Some(true) => {
                    vec![meta(ctx, ts, "model_only_prompt", task, Map::new())]
                }
                Some(task) => vec![event(ctx, ts, "user", task)],
                None => Vec::new(),
            },
            "final" => text_field(data, "text")
                .map(|text| event(ctx, ts, "assistant", text))
                .into_iter()
                .collect(),
            "tool_call" => vec![tool_call(
                ctx,
                ts,
                data.get("tool").and_then(Value::as_str),
                as_text(data.get("input")),
                record.get("eventId").and_then(Value::as_str),
            )],
            "tool_result" => {
                let result = data.get("result");
                vec![tool_result(
                    ctx,
                    ts,
                    data.get("tool").and_then(Value::as_str),
                    as_text(result),
                    record.get("parentId").and_then(Value::as_str),
                    result
                        .and_then(|result| result.get("ok"))
                        .and_then(Value::as_bool)
                        .map(|ok| !ok),
                )]
            }
            "action" | "assistant_raw" => Vec::new(),
            "run_error" => {
                let mut fields = Map::new();
                if let Some(operation) = data.get("operation") {
                    fields.insert("operation".into(), operation.clone());
                }
                vec![meta(ctx, ts, kind, as_text(data.get("message")), fields)]
            }
            "agent_state" if text_field(data, "purpose").is_some() => {
                let mut fields = Map::new();
                fields.insert("purpose".into(), data["purpose"].clone());
                vec![meta(ctx, ts, "delegated_session", None, fields)]
            }
            other => vec![meta(ctx, ts, other, None, Map::new())],
        }
    }
}
