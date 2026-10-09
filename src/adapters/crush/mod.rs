//! Adapter: Crush (Charm) —
//!   `<project>/<data dir>/crush.db` per project (`.crush` unless configured),
//!   every project Crush opened listed in `~/.local/share/crush/projects.json`
//!   as `{projects: [{path, data_dir, last_accessed}]}`.
//!
//! Source: charmbracelet/crush `internal/projects/projects.go` (the project
//! list beside the global `crush.json` data file), `internal/db/sql/
//! messages.sql` (messages: id, session_id, role, parts, model, provider,
//! created_at in seconds from `strftime('%s','now')`, finished_at) and
//! `internal/message/` (`parts` is a JSON list of `{type, data}` with type
//! text {text, hidden?}, reasoning {thinking}, tool_call {id, name, input},
//! tool_result {tool_call_id, name, content, is_error}, finish {reason, time},
//! image_url, binary). Sessions live in `sessions (id, title, ...)`.
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use super::common::{
    as_text, database_error, database_of, entry, event, is_file, iso_seconds, meta, open_database,
    read_json, text_field, tool_call, tool_result,
};
use crate::types::{Adapter, ParserCtx, RawEvent, Reading, SessionEntry};
use crate::util::Result;

const DATABASE: &str = "crush.db";

pub struct Crush;

impl Adapter for Crush {
    fn runtime(&self) -> &'static str {
        "crush"
    }

    fn roots(&self, home: &Path) -> Vec<PathBuf> {
        let Ok(list) = read_json(
            &home
                .join(".local")
                .join("share")
                .join("crush")
                .join("projects.json"),
        ) else {
            return Vec::new();
        };
        list.get("projects")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|project| {
                let path = PathBuf::from(text_field(project, "path")?);
                let data = PathBuf::from(text_field(project, "data_dir")?);
                Some(if data.is_absolute() {
                    data
                } else {
                    path.join(data)
                })
            })
            .filter(|data| data.is_dir())
            .collect()
    }

    fn list_sessions(&self, root: &Path) -> Vec<SessionEntry> {
        let database = root.join(DATABASE);
        let project = root
            .parent()
            .map(|project| project.to_string_lossy().to_string());
        if is_file(&database) {
            vec![entry(database, None, project)]
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

const MESSAGES: &str =
    "SELECT role, parts, model, created_at, session_id FROM messages ORDER BY session_id, created_at, rowid";

fn load(ctx: &ParserCtx) -> Result<Vec<RawEvent>> {
    let path = &ctx.file;
    let database = open_database(path)?;
    let mut statement = database
        .prepare(MESSAGES)
        .map_err(|error| database_error(path, "its messages", error))?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>("role")?,
                row.get::<_, String>("parts")?,
                row.get::<_, Option<String>>("model")?,
                row.get::<_, i64>("created_at")?,
                row.get::<_, String>("session_id")?,
            ))
        })
        .map_err(|error| database_error(path, "its messages", error))?;
    let mut events = Vec::new();
    for row in rows {
        let (role, parts, model, created, session) =
            row.map_err(|error| database_error(path, "a message", error))?;
        let Ok(parts) = serde_json::from_str::<Value>(&parts) else {
            continue;
        };
        let ts = iso_seconds(created as f64);
        let mut found = part_events(ctx, &role, &parts, ts);
        for event in &mut found {
            event.session_id = Some(session.clone());
            if event.event_type == "assistant" {
                event.model = model.clone();
            }
        }
        events.extend(found);
    }
    Ok(events)
}

fn part_events(ctx: &ParserCtx, role: &str, parts: &Value, ts: Option<String>) -> Vec<RawEvent> {
    let speaker = if role == "assistant" {
        "assistant"
    } else {
        "user"
    };
    let mut events = Vec::new();
    for part in parts.as_array().into_iter().flatten() {
        let Some(data) = part.get("data") else {
            continue;
        };
        match part.get("type").and_then(Value::as_str) {
            Some("text") => {
                let hidden = data.get("hidden").and_then(Value::as_bool) == Some(true);
                match text_field(data, "text") {
                    Some(text) if hidden => {
                        events.push(meta(ctx, ts.clone(), "hidden_text", text, Map::new()))
                    }
                    Some(text) => events.push(event(ctx, ts.clone(), speaker, text)),
                    None => {}
                }
            }
            Some("reasoning") => {
                if let Some(thought) = text_field(data, "thinking") {
                    events.push(event(ctx, ts.clone(), "thinking", thought));
                }
            }
            Some("tool_call") => events.push(tool_call(
                ctx,
                ts.clone(),
                data.get("name").and_then(Value::as_str),
                as_text(data.get("input")),
                data.get("id").and_then(Value::as_str),
            )),
            Some("tool_result") => events.push(tool_result(
                ctx,
                ts.clone(),
                data.get("name").and_then(Value::as_str),
                as_text(data.get("content")),
                data.get("tool_call_id").and_then(Value::as_str),
                data.get("is_error").and_then(Value::as_bool),
            )),
            Some("finish") => {
                let mut fields = Map::new();
                if let Some(reason) = data.get("reason") {
                    fields.insert("reason".into(), reason.clone());
                }
                events.push(meta(
                    ctx,
                    ts.clone(),
                    "finish",
                    as_text(data.get("message")),
                    fields,
                ));
            }
            Some(other) => events.push(meta(ctx, ts.clone(), other, String::new(), Map::new())),
            None => {}
        }
    }
    events
}
