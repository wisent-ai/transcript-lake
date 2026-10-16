//! Adapter: Devin CLI (Cognition's local `devin`) —
//!   `$XDG_DATA_HOME/devin/cli/sessions.db`, or the native default
//!   `~/.local/share/devin/cli/sessions.db`, one SQLite store for every
//!   session: `sessions (id, working_directory, hidden, main_chain_id, ...)`
//!   and `message_nodes (session_id, node_id, parent_node_id, chat_message,
//!   created_at (unix seconds), metadata)`.
//!
//! Source: the observed-format registry vshulcz/deja-vu
//! `docs/registry/devin.md` and its SQL fixture (Devin CLI 3000.11.3). A
//! node's `chat_message` is `{message_id, role: system | user | assistant |
//! tool, content, tool_calls: [{id, name, arguments}], tool_call_id}`. The
//! node table in order is not the conversation: `chains` rebuilds the chain
//! `sessions.main_chain_id` heads, each chain a `subagent_heads` row
//! declares, and each branch a rewind left behind, reading a repeated
//! `message_id` once at its first copy, when it was said. Nodes marked
//! `is_system_prefix` are the rebuilt scaffolding; sessions marked `hidden`
//! were removed from view by their owner and are not read. A side chain's
//! events carry the parent session in `extra.parent_session` and the chain's
//! name in `extra.chain`.
mod chains;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

use chains::{Identity, Node};

use super::common::{
    chat_messages, database_error, database_of, entry, existing_root, is_file, iso_seconds,
    open_database, text_field, text_of,
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
        let data = match std::env::var_os("XDG_DATA_HOME") {
            Some(path) if !path.is_empty() => PathBuf::from(path),
            _ => home.join(".local").join("share"),
        };
        existing_root(data.join("devin").join("cli"))
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

const SESSIONS: &str = "SELECT id, working_directory, main_chain_id FROM sessions \
     WHERE hidden = 0 ORDER BY id";

const NODES: &str = "SELECT node_id, parent_node_id, chat_message, created_at, metadata \
     FROM message_nodes WHERE session_id = ?1 ORDER BY node_id";

const HEADS: &str =
    "SELECT agent_id, chain_node_id FROM subagent_heads WHERE session_id = ?1 ORDER BY agent_id";

/// One stored node with its parsed message.
struct Stored {
    node: Node,
    message: Value,
    created: Option<i64>,
}

fn stored_nodes(
    database: &rusqlite::Connection,
    path: &Path,
    session: &str,
) -> Result<BTreeMap<i64, Stored>> {
    let mut statement = database
        .prepare(NODES)
        .map_err(|error| database_error(path, "its message nodes", error))?;
    let rows = statement
        .query_map([session], |row| {
            Ok((
                row.get::<_, i64>("node_id")?,
                row.get::<_, Option<i64>>("parent_node_id")?,
                row.get::<_, String>("chat_message")?,
                row.get::<_, Option<i64>>("created_at")?,
                row.get::<_, Option<String>>("metadata")?,
            ))
        })
        .map_err(|error| database_error(path, "its message nodes", error))?;
    let mut nodes = BTreeMap::new();
    for row in rows {
        let (id, parent, message, created, metadata) =
            row.map_err(|error| database_error(path, "a message node", error))?;
        let metadata = metadata
            .map(|metadata| {
                serde_json::from_str::<Value>(&metadata).map_err(|error| {
                    crate::util::Error(format!(
                        "the metadata of node {id} of session {session} in {} is not JSON: {error}",
                        path.display()
                    ))
                })
            })
            .transpose()?;
        let scaffolding = metadata
            .and_then(|metadata| metadata.get("is_system_prefix").and_then(Value::as_bool))
            == Some(true);
        let message = serde_json::from_str::<Value>(&message).map_err(|error| {
            crate::util::Error(format!(
                "the message of node {id} of session {session} in {} is not JSON: {error}",
                path.display()
            ))
        })?;
        let message_id = text_field(&message, "message_id");
        nodes.insert(
            id,
            Stored {
                node: Node {
                    parent,
                    message_id,
                    scaffolding,
                },
                message,
                created,
            },
        );
    }
    Ok(nodes)
}

fn subagent_heads(
    database: &rusqlite::Connection,
    path: &Path,
    session: &str,
) -> Result<BTreeMap<String, i64>> {
    let mut statement = database
        .prepare(HEADS)
        .map_err(|error| database_error(path, "its subagent heads", error))?;
    let rows = statement
        .query_map([session], |row| {
            Ok((
                row.get::<_, String>("agent_id")?,
                row.get::<_, i64>("chain_node_id")?,
            ))
        })
        .map_err(|error| database_error(path, "its subagent heads", error))?;
    rows.collect::<std::result::Result<BTreeMap<_, _>, _>>()
        .map_err(|error| database_error(path, "a subagent head", error))
}

fn load(ctx: &ParserCtx) -> Result<Vec<RawEvent>> {
    let path = &ctx.file;
    let database = open_database(path)?;
    let mut statement = database
        .prepare(SESSIONS)
        .map_err(|error| database_error(path, "its sessions", error))?;
    let sessions = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>("id")?,
                row.get::<_, Option<String>>("working_directory")?,
                row.get::<_, Option<i64>>("main_chain_id")?,
            ))
        })
        .map_err(|error| database_error(path, "its sessions", error))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|error| database_error(path, "a session", error))?;
    let mut events = Vec::new();
    for (session, directory, main_head) in sessions {
        let stored = stored_nodes(&database, path, &session)?;
        let nodes: BTreeMap<i64, Node> = stored
            .iter()
            .map(|(id, stored)| (*id, stored.node.clone()))
            .collect();
        let heads = subagent_heads(&database, path, &session)?;
        let chains = chains::chains(&nodes, main_head, &heads).map_err(|detail| {
            crate::util::Error(format!(
                "{}: session {session}: its conversation chains cannot be rebuilt: {detail}",
                path.display()
            ))
        })?;
        for chain in chains {
            let (session_id, chain_name) = match &chain.identity {
                Identity::Main => (session.clone(), None),
                Identity::Subagent(agent) => (format!("{session}:{agent}"), Some(agent.clone())),
                Identity::Branch(root) => (
                    format!("{session}:branch-{root}"),
                    Some(format!("branch-{root}")),
                ),
            };
            for id in chain.nodes {
                let node = &stored[&id];
                let message = &node.message;
                let mut found = chat_messages(
                    ctx,
                    std::slice::from_ref(message),
                    node.created.and_then(|seconds| iso_seconds(seconds as f64)),
                );
                for event in &mut found {
                    event.session_id = Some(session_id.clone());
                    event.project = directory.clone();
                    if message.get("role").and_then(Value::as_str) == Some("system") {
                        event.text = text_of(message.get("content"));
                    }
                    if let Some(name) = &chain_name {
                        event
                            .extra
                            .insert("parent_session".to_string(), Value::String(session.clone()));
                        event
                            .extra
                            .insert("chain".to_string(), Value::String(name.clone()));
                    }
                }
                events.extend(found);
            }
        }
    }
    Ok(events)
}
