//! Adapter: Hermes (Nous Research's agent) —
//!   `~/.hermes/state.db` (0.17+) and `~/.hermes/profiles/<profile>/state.db`
//!   (older builds, one store per profile), SQLite: a flat `messages
//!   (session_id, role, content, tool_call_id, tool_calls, tool_name,
//!   timestamp REAL epoch seconds, active?, compacted?)` table, beside
//!   `sessions (id, cwd, ...)` in newer stores.
//!
//! Source: the observed-format registry vshulcz/deja-vu
//! `docs/registry/hermes.md` and its SQL fixture (Hermes 0.17.0). An
//! assistant row that calls tools has an OpenAI-style `tool_calls` array; a
//! tool row answers one under `tool_call_id`. Multimodal content is stored as
//! `\x00json:` and a list of parts; such content that is not JSON is kept as
//! the text it is. Rows a rewind or compaction took out of the live
//! conversation (`active = 0`, `compacted = 0`) were still said, and are
//! archived like the rest.
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::common::{
    chat_messages, database_error, database_of, entry, existing_root, is_file, iso_seconds,
    open_database, subdirectories,
};
use crate::types::{Adapter, ParserCtx, RawEvent, Reading, SessionEntry};
use crate::util::Result;

const DATABASE: &str = "state.db";
/// The prefix Hermes stores multimodal content behind.
const PARTS_PREFIX: &str = "\u{0}json:";

pub struct Hermes;

impl Adapter for Hermes {
    fn runtime(&self) -> &'static str {
        "hermes"
    }

    fn roots(&self, home: &Path) -> Vec<PathBuf> {
        let base = home.join(".hermes");
        let mut roots = existing_root(base.clone());
        roots.extend(subdirectories(&base.join("profiles")));
        roots
    }

    fn watch_roots(&self, home: &Path) -> Vec<PathBuf> {
        existing_root(home.join(".hermes"))
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

/// The tables a store has.
fn tables(connection: &rusqlite::Connection, path: &Path) -> Result<HashSet<String>> {
    let mut statement = connection
        .prepare("SELECT name FROM sqlite_master WHERE type = 'table'")
        .map_err(|error| database_error(path, "its tables", error))?;
    let names = statement
        .query_map([], |row| row.get::<_, String>("name"))
        .map_err(|error| database_error(path, "its tables", error))?;
    names
        .collect::<std::result::Result<_, _>>()
        .map_err(|error| database_error(path, "a table name", error))
}

/// A message's content: its text, or the parts list Hermes stored behind
/// its multimodal prefix.
fn content_of(text: String) -> Value {
    let parts = text
        .strip_prefix(PARTS_PREFIX)
        .and_then(|parts| serde_json::from_str::<Value>(parts).ok());
    match parts {
        Some(parts) => parts,
        None => Value::String(text),
    }
}

fn load(ctx: &ParserCtx) -> Result<Vec<RawEvent>> {
    let path = &ctx.file;
    let database = open_database(path)?;
    let directory = if tables(&database, path)?.contains("sessions") {
        "(SELECT cwd FROM sessions WHERE sessions.id = messages.session_id)"
    } else {
        "NULL"
    };
    let query = format!(
        "SELECT session_id, role, content, tool_call_id, tool_calls, tool_name, timestamp, {directory} AS directory \
         FROM messages ORDER BY rowid"
    );
    let mut statement = database
        .prepare(&query)
        .map_err(|error| database_error(path, "its messages", error))?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>("session_id")?,
                row.get::<_, String>("role")?,
                row.get::<_, Option<String>>("content")?,
                row.get::<_, Option<String>>("tool_call_id")?,
                row.get::<_, Option<String>>("tool_calls")?,
                row.get::<_, Option<String>>("tool_name")?,
                row.get::<_, f64>("timestamp")?,
                row.get::<_, Option<String>>("directory")?,
            ))
        })
        .map_err(|error| database_error(path, "its messages", error))?;
    let mut events = Vec::new();
    for row in rows {
        let (session, role, content, call_id, calls, tool, stamp, directory) =
            row.map_err(|error| database_error(path, "a message", error))?;
        let message = serde_json::json!({
            "role": role,
            "content": content.map(content_of),
            "tool_call_id": call_id,
            "name": tool,
            "tool_calls": calls.and_then(|calls| serde_json::from_str::<Value>(&calls).ok()),
        });
        let mut found = chat_messages(ctx, &[message], iso_seconds(stamp));
        for event in &mut found {
            event.session_id = Some(session.clone());
            event.project = directory.clone();
        }
        events.extend(found);
    }
    Ok(events)
}
