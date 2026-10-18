//! Adapter: Qwen Code —
//!   `~/.qwen/projects/<sanitized-cwd>/chats/<session-id>.jsonl`, and the
//!   `~/.qwen/tmp/<project-id>/chats/` layout earlier releases wrote.
//!
//! Source: QwenLM/qwen-code `packages/core/src/services/
//! chatRecordingService.ts` (`ChatRecord`), and the records Oko's decoder
//! reads (`oko/rust/src/decode/google.rs`). Each line is appended once:
//!   `{uuid, parentUuid, sessionId, timestamp, type, subtype?, cwd, message?:
//!   {role, parts}, usageMetadata?, model?, toolCallResult?: {callId,
//!   responseParts, resultDisplay, error, status}, systemPayload?}` with type
//!   user | assistant | tool_result | system.
//! A user-role record is authored only when the source explicitly marks
//! `provenance: real_user`. Qwen also writes goal, agent and system material
//! as user-role records; a missing provenance is unknown, not an author.
//! Non-author user-role records remain metadata with their source labels.
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use super::gemini::chat_roots;
use super::parts::{part_events, parts, usage_metadata};
use crate::adapters::common::{
    appended_record, as_text, entry, existing_root, file_name, files_with_suffix, is_file, meta,
    text_field, tool_result,
};
use crate::types::{Adapter, Parser, ParserCtx, RawEvent, Reading, SessionEntry};

pub struct Qwen;

impl Adapter for Qwen {
    fn runtime(&self) -> &'static str {
        "qwen"
    }

    fn roots(&self, home: &Path) -> Vec<PathBuf> {
        let base = home.join(".qwen");
        let mut roots = chat_roots(&base.join("projects"));
        roots.extend(chat_roots(&base.join("tmp")));
        roots
    }

    fn watch_roots(&self, home: &Path) -> Vec<PathBuf> {
        let base = home.join(".qwen");
        let mut roots = existing_root(base.join("projects"));
        roots.extend(existing_root(base.join("tmp")));
        roots
    }

    fn list_sessions(&self, root: &Path) -> Vec<SessionEntry> {
        files_with_suffix(root, ".jsonl")
            .iter()
            .filter_map(|file| session_entry(file))
            .collect()
    }

    fn entry_for(&self, path: &Path) -> Option<SessionEntry> {
        let chats = path.parent()?;
        if !self
            .roots(&crate::util::home_dir())
            .iter()
            .any(|root| root.as_path() == chats)
            || !is_file(path)
        {
            return None;
        }
        session_entry(path)
    }

    fn read(&self, ctx: ParserCtx) -> Reading {
        Reading::Lines(Box::new(QwenParser { ctx }))
    }
}

fn session_entry(file: &Path) -> Option<SessionEntry> {
    let name = file_name(file)?;
    let id = name.strip_suffix(".jsonl")?;
    Some(entry(file.to_path_buf(), Some(id.to_string()), None))
}

struct QwenParser {
    ctx: ParserCtx,
}

impl Parser for QwenParser {
    fn on_line(&mut self, line: &str) -> Vec<RawEvent> {
        let Some(record) = appended_record(&self.ctx.file, line) else {
            return Vec::new();
        };
        let ts = text_field(&record, "timestamp");
        let subtype = record.get("subtype").and_then(Value::as_str);
        let provenance = text_field(&record, "provenance");
        let content = parts(record.get("message"));
        let ctx = &self.ctx;
        let message_role = record
            .get("message")
            .and_then(|message| message.get("role"))
            .and_then(Value::as_str);
        let mut events = match (record.get("type").and_then(Value::as_str), subtype) {
            (Some("user"), _) if message_role == Some("tool") => {
                part_events(ctx, "tool_result", &content, ts.clone())
            }
            (Some("user"), _) if provenance.as_deref() == Some("real_user") => {
                let mut events = part_events(ctx, "user", &content, ts.clone());
                for event in &mut events {
                    if event.event_type == "user" {
                        event
                            .extra
                            .insert("attribution".into(), Value::String("user".into()));
                        event
                            .extra
                            .insert("provenance".into(), Value::String("real_user".into()));
                    }
                }
                events
            }
            (Some("user"), _) => {
                let words = content
                    .iter()
                    .map(|part| as_text(part.get("text")))
                    .collect::<Vec<_>>()
                    .join("\n");
                let mut fields = Map::new();
                if let Some(written) = subtype {
                    fields.insert("subtype".into(), Value::String(written.to_owned()));
                }
                if let Some(source) = provenance.as_deref() {
                    fields.insert("provenance".into(), Value::String(source.to_owned()));
                }
                vec![meta(ctx, ts.clone(), "unattributed_user", words, fields)]
            }
            (Some("assistant"), _) => {
                let mut events = part_events(ctx, "assistant", &content, ts.clone());
                if let Some(metadata) = record
                    .get("usageMetadata")
                    .filter(|metadata| metadata.is_object())
                {
                    events.push(usage_metadata(
                        ctx,
                        metadata,
                        text_field(&record, "model"),
                        ts.clone(),
                    ));
                }
                events
            }
            (Some("tool_result"), _) if !content.is_empty() => {
                part_events(ctx, "tool_result", &content, ts.clone())
            }
            (Some("tool_result"), _) => {
                let result = record.get("toolCallResult");
                vec![tool_result(
                    ctx,
                    ts.clone(),
                    None,
                    as_text(result.and_then(|result| result.get("resultDisplay"))),
                    result
                        .and_then(|result| result.get("callId"))
                        .and_then(Value::as_str),
                    result
                        .and_then(|result| result.get("status"))
                        .and_then(Value::as_str)
                        .map(|status| status == "error"),
                )]
            }
            (Some("system"), _) => {
                let mut fields = Map::new();
                if let Some(written) = subtype {
                    fields.insert("subtype".into(), Value::String(written.to_string()));
                }
                vec![meta(ctx, ts.clone(), "system", None, fields)]
            }
            _ => Vec::new(),
        };
        let project = text_field(&record, "cwd");
        let session = text_field(&record, "sessionId");
        for event in &mut events {
            if project.is_some() {
                event.project = project.clone();
            }
            if session.is_some() {
                event.session_id = session.clone();
            }
        }
        events
    }
}
