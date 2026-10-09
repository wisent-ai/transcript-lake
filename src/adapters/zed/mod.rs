//! Adapter: Zed's agent threads —
//!   `<Zed data dir>/threads/threads.db` (`~/Library/Application Support/Zed`
//!   on macOS, `~/.local/share/zed` elsewhere), table `threads (id,
//!   parent_id, folder_paths, summary, updated_at, created_at, data_type,
//!   data)`; `data` is a thread's JSON, zstd-compressed when `data_type` is
//!   `zstd`.
//!
//! Source: zed-industries/zed `crates/agent/src/db.rs` (`ThreadsDatabase`,
//! `DataType`, `DbThread {title, messages, updated_at, model, ...}`) and
//! `crates/agent/src/thread.rs`: `Message` is `{"User": {id, content}}`,
//! `{"Agent": {content, tool_results, reasoning_details}}` or `"Resume"`;
//! user content is `{"Text": ...}`, `{"Mention": {uri, content}}` or an
//! image; agent content is `{"Text": ...}`, `{"Thinking": {text,
//! signature}}`, `{"RedactedThinking": ...}` or `{"ToolUse": {id, name,
//! raw_input, input}}`; `tool_results` maps a tool use id to `{tool_use_id,
//! tool_name, is_error, content: [{"Text": ...} | image], output}`.
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use super::common::{
    as_text, database_error, database_of, entry, event, existing_root, is_file, meta,
    open_database, tool_call, tool_result,
};
use crate::types::{Adapter, ParserCtx, RawEvent, Reading, SessionEntry};
use crate::util::{Error, Result};

const DATABASE: &str = "threads.db";

pub struct Zed;

impl Adapter for Zed {
    fn runtime(&self) -> &'static str {
        "zed"
    }

    fn roots(&self, home: &Path) -> Vec<PathBuf> {
        let mut roots = existing_root(
            home.join("Library")
                .join("Application Support")
                .join("Zed")
                .join("threads"),
        );
        roots.extend(existing_root(
            home.join(".local")
                .join("share")
                .join("zed")
                .join("threads"),
        ));
        roots
    }

    fn list_sessions(&self, root: &Path) -> Vec<SessionEntry> {
        let database = root.join(DATABASE);
        if is_file(&database) {
            vec![entry(database, None, None)]
        } else {
            Vec::new()
        }
    }

    fn entry_for(&self, path: &Path) -> Option<SessionEntry> {
        let database = database_of(path);
        let root = database.parent()?;
        if !self
            .roots(&crate::util::home_dir())
            .iter()
            .any(|known| known.as_path() == root)
        {
            return None;
        }
        self.list_sessions(root)
            .into_iter()
            .find(|session| session.file == database)
    }

    fn read(&self, ctx: ParserCtx) -> Reading {
        Reading::Whole(Box::new(move || load(&ctx)))
    }
}

const THREADS: &str =
    "SELECT id, folder_paths, updated_at, data_type, data FROM threads ORDER BY updated_at, id";

fn load(ctx: &ParserCtx) -> Result<Vec<RawEvent>> {
    let path = &ctx.file;
    let database = open_database(path)?;
    let mut statement = database
        .prepare(THREADS)
        .map_err(|error| database_error(path, "its threads", error))?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>("id")?,
                row.get::<_, Option<String>>("folder_paths")?,
                row.get::<_, String>("updated_at")?,
                row.get::<_, String>("data_type")?,
                row.get::<_, Vec<u8>>("data")?,
            ))
        })
        .map_err(|error| database_error(path, "its threads", error))?;
    let mut events = Vec::new();
    for row in rows {
        let (id, folders, updated, data_type, data) =
            row.map_err(|error| database_error(path, "a thread", error))?;
        let json = if data_type == "zstd" {
            zstd::decode_all(data.as_slice()).map_err(|error| {
                Error(format!(
                    "thread {id} in {} cannot be decompressed: {error}",
                    path.display()
                ))
            })?
        } else {
            data
        };
        let Ok(thread) = serde_json::from_slice::<Value>(&json) else {
            continue;
        };
        let project = folders.and_then(|folders| folders.lines().next().map(str::to_string));
        let mut found = thread_events(ctx, &thread, Some(updated));
        for event in &mut found {
            event.session_id = Some(id.clone());
            event.project = project.clone();
        }
        events.extend(found);
    }
    Ok(events)
}

/// The text of `{"Text": ...}` content, or of a bare string.
fn text_variant(content: &Value) -> Option<String> {
    match content {
        Value::String(text) => Some(text.clone()),
        Value::Object(map) => map.get("Text").and_then(Value::as_str).map(str::to_string),
        _ => None,
    }
}

fn thread_events(ctx: &ParserCtx, thread: &Value, ts: Option<String>) -> Vec<RawEvent> {
    let mut events = Vec::new();
    for message in thread
        .get("messages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if let Some(user) = message.get("User") {
            for content in user
                .get("content")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if let Some(text) = text_variant(content).filter(|text| !text.is_empty()) {
                    events.push(event(ctx, ts.clone(), "user", text));
                } else if let Some(mention) = content.get("Mention") {
                    events.push(meta(
                        ctx,
                        ts.clone(),
                        "mention",
                        as_text(mention.get("uri")),
                        Map::new(),
                    ));
                }
            }
        } else if let Some(agent) = message.get("Agent") {
            for content in agent
                .get("content")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if let Some(text) = text_variant(content).filter(|text| !text.is_empty()) {
                    events.push(event(ctx, ts.clone(), "assistant", text));
                } else if let Some(thinking) = content.get("Thinking") {
                    events.push(event(
                        ctx,
                        ts.clone(),
                        "thinking",
                        as_text(thinking.get("text")),
                    ));
                } else if let Some(tool) = content.get("ToolUse") {
                    events.push(tool_call(
                        ctx,
                        ts.clone(),
                        tool.get("name").and_then(Value::as_str),
                        as_text(tool.get("raw_input")),
                        tool.get("id").and_then(Value::as_str),
                    ));
                }
            }
            for result in agent
                .get("tool_results")
                .and_then(Value::as_object)
                .into_iter()
                .flat_map(Map::values)
            {
                let output = result
                    .get("content")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(text_variant)
                    .collect::<Vec<_>>()
                    .join("\n");
                events.push(tool_result(
                    ctx,
                    ts.clone(),
                    result.get("tool_name").and_then(Value::as_str),
                    output,
                    result.get("tool_use_id").and_then(Value::as_str),
                    result.get("is_error").and_then(Value::as_bool),
                ));
            }
        }
    }
    events
}
