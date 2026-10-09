//! Adapter: Devin CLI (Cognition's local `devin`) —
//!   `~/.local/share/devin/cli/sessions.db`, one SQLite store for every
//!   session: `sessions (id, working_directory, hidden, main_chain_id, ...)`
//!   and `message_nodes (session_id, node_id, parent_node_id, chat_message,
//!   created_at (unix seconds), metadata)`.
//!
//! Source: the observed-format registry vshulcz/deja-vu
//! `docs/registry/devin.md` and its SQL fixture (Devin CLI 3000.11.3). A
//! node's `chat_message` is `{message_id, role: system | user | assistant |
//! tool, content, tool_calls: [{id, name, arguments}], tool_call_id}`. The
//! agent rebuilds its context chain whenever its system prefix changes, so
//! several nodes repeat one `message_id`: each message is read once, at its
//! first node. Nodes marked `is_system_prefix` are the rebuilt scaffolding;
//! sessions marked `hidden` were removed from view by their owner and are
//! not read.
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::common::{
    chat_messages, database_error, database_of, entry, existing_root, is_file, iso_seconds,
    open_database, text_field,
};
use crate::types::{Adapter, ParserCtx, RawEvent, Reading, SessionEntry};
use crate::util::Result;

const DATABASE: &str = "sessions.db";

pub struct Devin;

impl Adapter for Devin {
    fn runtime(&self) -> &'static str {
        "devin"
    }

    fn roots(&self, home: &Path) -> Vec<PathBuf> {
        existing_root(home.join(".local").join("share").join("devin").join("cli"))
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

const NODES: &str =
    "SELECT message_nodes.session_id AS session_id, message_nodes.chat_message AS chat_message, \
     message_nodes.created_at AS created_at, message_nodes.metadata AS metadata, \
     sessions.working_directory AS directory \
     FROM message_nodes JOIN sessions ON sessions.id = message_nodes.session_id \
     WHERE sessions.hidden = 0 ORDER BY message_nodes.session_id, message_nodes.node_id";

fn load(ctx: &ParserCtx) -> Result<Vec<RawEvent>> {
    let path = &ctx.file;
    let database = open_database(path)?;
    let mut statement = database
        .prepare(NODES)
        .map_err(|error| database_error(path, "its message nodes", error))?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>("session_id")?,
                row.get::<_, String>("chat_message")?,
                row.get::<_, Option<i64>>("created_at")?,
                row.get::<_, Option<String>>("metadata")?,
                row.get::<_, Option<String>>("directory")?,
            ))
        })
        .map_err(|error| database_error(path, "its message nodes", error))?;
    let mut seen: HashSet<(String, String)> = HashSet::new();
    let mut events = Vec::new();
    for row in rows {
        let (session, message, created, metadata, directory) =
            row.map_err(|error| database_error(path, "a message node", error))?;
        let scaffolding = metadata
            .and_then(|metadata| serde_json::from_str::<Value>(&metadata).ok())
            .and_then(|metadata| metadata.get("is_system_prefix").and_then(Value::as_bool))
            == Some(true);
        let Ok(message) = serde_json::from_str::<Value>(&message) else {
            continue;
        };
        if scaffolding {
            continue;
        }
        if let Some(id) = text_field(&message, "message_id") {
            if !seen.insert((session.clone(), id)) {
                continue;
            }
        }
        let mut found = chat_messages(
            ctx,
            &[message],
            created.and_then(|seconds| iso_seconds(seconds as f64)),
        );
        for event in &mut found {
            event.session_id = Some(session.clone());
            event.project = directory.clone();
        }
        events.extend(found);
    }
    Ok(events)
}
