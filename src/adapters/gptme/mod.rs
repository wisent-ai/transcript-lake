//! Adapter: gptme —
//!   `<data dir>/logs/<conversation name>/conversation.jsonl`, appended one
//!   message per line, where the data dir is platformdirs'
//!   `user_data_dir("gptme")`: `~/Library/Application Support/gptme` on
//!   macOS, `~/.local/share/gptme` elsewhere.
//!
//! Source: gptme/gptme `gptme/dirs.py` (`get_data_dir`, `get_logs_dir`) and
//! `gptme/message.py` (`Message.to_dict`: `{role, content, timestamp,
//! files?, pinned?, hide?, ui_only?, call_id?, metadata?: {usage?, model?}}`).
//! gptme runs tools from code blocks in the assistant's text and records
//! their output as `system` messages, so a system message is a tool result
//! when it answers a call (`call_id`) and metadata otherwise; `hide` and
//! `ui_only` messages are the harness's own and become metadata too.
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::adapters::common::{
    entry, event, int_field, is_file, meta, subdirectories, text_field, tool_result, usage,
};
use crate::types::{Adapter, Parser, ParserCtx, RawEvent, Reading, SessionEntry};

const CONVERSATION: &str = "conversation.jsonl";

pub struct Gptme;

impl Adapter for Gptme {
    fn runtime(&self) -> &'static str {
        "gptme"
    }

    fn roots(&self, home: &Path) -> Vec<PathBuf> {
        [
            home.join("Library")
                .join("Application Support")
                .join("gptme"),
            home.join(".local").join("share").join("gptme"),
        ]
        .into_iter()
        .map(|data| data.join("logs"))
        .filter(|logs| logs.is_dir())
        .collect()
    }

    fn list_sessions(&self, root: &Path) -> Vec<SessionEntry> {
        subdirectories(root)
            .iter()
            .filter_map(|conversation| session_entry(conversation))
            .collect()
    }

    fn entry_for(&self, path: &Path) -> Option<SessionEntry> {
        if path.file_name()? != CONVERSATION {
            return None;
        }
        let conversation = path.parent()?;
        if !self
            .roots(&crate::util::home_dir())
            .iter()
            .any(|root| conversation.parent() == Some(root.as_path()))
        {
            return None;
        }
        session_entry(conversation)
    }

    fn read(&self, ctx: ParserCtx) -> Reading {
        Reading::Lines(Box::new(GptmeParser { ctx }))
    }
}

fn session_entry(conversation: &Path) -> Option<SessionEntry> {
    let file = conversation.join(CONVERSATION);
    if !is_file(&file) {
        return None;
    }
    Some(entry(
        file,
        Some(conversation.file_name()?.to_string_lossy().to_string()),
        None,
    ))
}

struct GptmeParser {
    ctx: ParserCtx,
}

impl Parser for GptmeParser {
    fn on_line(&mut self, line: &str) -> Vec<RawEvent> {
        let Ok(message) = serde_json::from_str::<Value>(line) else {
            return Vec::new();
        };
        let ctx = &self.ctx;
        let ts = text_field(&message, "timestamp");
        let Some(content) = text_field(&message, "content") else {
            return Vec::new();
        };
        let flagged = |key: &str| message.get(key).and_then(Value::as_bool) == Some(true);
        let call = message.get("call_id").and_then(Value::as_str);
        let mut events = match message.get("role").and_then(Value::as_str) {
            Some(role) if flagged("hide") || flagged("ui_only") => {
                let mut fields = Map::new();
                fields.insert("role".into(), Value::String(role.to_string()));
                vec![meta(ctx, ts.clone(), "hidden_message", content, fields)]
            }
            Some("user") => vec![event(ctx, ts.clone(), "user", content)],
            Some("assistant") => vec![event(ctx, ts.clone(), "assistant", content)],
            Some("system") if call.is_some() => {
                vec![tool_result(ctx, ts.clone(), None, content, call, None)]
            }
            Some("system") => vec![meta(ctx, ts.clone(), "system", content, Map::new())],
            _ => Vec::new(),
        };
        if let Some(metadata) = message.get("metadata") {
            let spent = metadata.get("usage");
            let count = |key: &str| spent.and_then(|spent| int_field(spent, key));
            if spent.is_some() {
                events.push(usage(
                    ctx,
                    ts,
                    text_field(metadata, "model"),
                    count("input_tokens"),
                    count("output_tokens"),
                ));
            }
        }
        events
    }
}
