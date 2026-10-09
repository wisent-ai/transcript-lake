//! Adapter: Mistral Vibe —
//!   `~/.vibe/logs/session/<prefix>_<timestamp>_<short id>/messages.jsonl`
//!   beside `meta.json` (`{session_id, start_time, end_time, environment:
//!   {working_directory}, origin_directory, title, config, stats, ...}`).
//!
//! Source: mistralai/mistral-vibe `vibe/core/session/session_logger.py`
//! (`save_folder`, `_persist_messages_sync`, `_overwrite_messages_sync`,
//! `_initialize_session_metadata`) and `session_index.py` (`meta.json`,
//! `messages.jsonl`); the home is `~/.vibe` (Mistral's configuration docs).
//! Each line is an `LLMMessage` dumped without nulls — the chat-completions
//! shape `{role, content, reasoning_content?, tool_calls?: [{id, function:
//! {name, arguments}}], tool_call_id?, name?}`; system messages are not
//! written. A rewind rewrites the file, so a session is read whole.
use std::path::{Path, PathBuf};

use super::common::{
    chat_messages, entry, existing_root, is_file, read_json, read_json_lines, subdirectories,
    text_field,
};
use crate::types::{Adapter, ParserCtx, RawEvent, Reading, SessionEntry};
use crate::util::Result;

const MESSAGES: &str = "messages.jsonl";
const METADATA: &str = "meta.json";

pub struct Vibe;

impl Adapter for Vibe {
    fn runtime(&self) -> &'static str {
        "vibe"
    }

    fn roots(&self, home: &Path) -> Vec<PathBuf> {
        existing_root(home.join(".vibe").join("logs").join("session"))
    }

    fn list_sessions(&self, root: &Path) -> Vec<SessionEntry> {
        subdirectories(root)
            .iter()
            .filter_map(|session| session_entry(session))
            .collect()
    }

    fn entry_for(&self, path: &Path) -> Option<SessionEntry> {
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
        Reading::Whole(Box::new(move || load(&ctx)))
    }
}

fn session_entry(session: &Path) -> Option<SessionEntry> {
    let messages = session.join(MESSAGES);
    if !is_file(&messages) {
        return None;
    }
    let metadata = read_json(&session.join(METADATA)).ok();
    let id = metadata
        .as_ref()
        .and_then(|metadata| text_field(metadata, "session_id"));
    let project = metadata
        .as_ref()
        .and_then(|metadata| metadata.get("environment"))
        .and_then(|environment| text_field(environment, "working_directory"));
    Some(entry(messages, id, project))
}

fn load(ctx: &ParserCtx) -> Result<Vec<RawEvent>> {
    let messages = read_json_lines(&ctx.file)?;
    let started = ctx
        .file
        .parent()
        .and_then(|session| read_json(&session.join(METADATA)).ok())
        .and_then(|metadata| text_field(&metadata, "start_time"));
    Ok(chat_messages(ctx, &messages, started))
}
