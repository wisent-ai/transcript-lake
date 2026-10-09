//! Adapter: OpenCode —
//!   `~/.local/share/opencode/opencode.db` (or `opencode-<channel>.db` for a
//!   non-release channel), one SQLite database in WAL mode holding every
//!   session; earlier releases' JSON files are migrated into it by OpenCode —
//!   and the harnesses forked from it that keep its schema: the Kilo CLI
//!   (`~/.local/share/kilo/kilo.db`) and the ZCode CLI
//!   (`~/.zcode/cli/db/db.sqlite`), per the observed-format registry
//!   vshulcz/deja-vu `docs/registry/{kilocode,zcode}.md` ("the CLI's
//!   OpenCode-schema SQLite").
//!
//! Source: anomalyco/opencode `packages/opencode/src/storage/db.ts`
//! (`getChannelPath`: `<Global.Path.data>/opencode.db`, WAL journal) and
//! `src/session/session.sql.ts`: tables
//!   session (id, project_id, parent_id, directory, title, time_created, ...),
//!   message (id, session_id, time_created, data: MessageV2.Info JSON —
//!     `{role, modelID?, providerID?, tokens?: {input, output, reasoning,
//!     cache}, path?: {cwd}}`),
//!   part (id, message_id, session_id, time_created, data: MessageV2.Part JSON
//!     — `{type: text, text, synthetic?}`, `{type: reasoning, text}`,
//!     `{type: tool, callID, tool, state: {status, input, output?, error?}}`,
//!     `{type: step-finish, tokens}`, and file / patch / agent / subtask /
//!     compaction / snapshot / step-start parts).
//! The database is one entry: every session's events are read whole, each
//! carrying its session id and directory.
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use super::common::{
    as_text, database_error, database_of, entry, event, existing_root, file_name,
    files_with_suffix, int_field, iso_millis, meta, open_database, text_field, tool_call,
    tool_result, usage,
};
use crate::types::{Adapter, ParserCtx, RawEvent, Reading, SessionEntry};
use crate::util::Result;

/// One harness that keeps OpenCode's database: its runtime id, its data
/// directory under the home, and how its database files are named.
pub struct OpenCodeStore {
    runtime: &'static str,
    data_dir: &'static [&'static str],
    database: &'static str,
    suffix: &'static str,
}

/// OpenCode itself.
pub const OPENCODE: OpenCodeStore = OpenCodeStore {
    runtime: "opencode",
    data_dir: &[".local", "share", "opencode"],
    database: "opencode",
    suffix: ".db",
};
/// The Kilo CLI.
pub const KILO: OpenCodeStore = OpenCodeStore {
    runtime: "kilo",
    data_dir: &[".local", "share", "kilo"],
    database: "kilo",
    suffix: ".db",
};
/// The ZCode CLI.
pub const ZCODE: OpenCodeStore = OpenCodeStore {
    runtime: "zcode",
    data_dir: &[".zcode", "cli", "db"],
    database: "db",
    suffix: ".sqlite",
};

impl Adapter for OpenCodeStore {
    fn runtime(&self) -> &'static str {
        self.runtime
    }

    fn roots(&self, home: &Path) -> Vec<PathBuf> {
        existing_root(
            self.data_dir
                .iter()
                .fold(home.to_path_buf(), |path, part| path.join(part)),
        )
    }

    fn list_sessions(&self, root: &Path) -> Vec<SessionEntry> {
        files_with_suffix(root, self.suffix)
            .into_iter()
            .filter(|file| file_name(file).is_some_and(|name| name.starts_with(self.database)))
            .map(|file| entry(file, None, None))
            .collect()
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

const PARTS: &str = "SELECT part.data AS part_data, part.session_id AS session_id, \
     part.time_created AS created, message.data AS message_data, session.directory AS directory \
     FROM part JOIN message ON message.id = part.message_id JOIN session ON session.id = part.session_id \
     ORDER BY part.session_id, message.time_created, part.time_created, part.id";

fn load(ctx: &ParserCtx) -> Result<Vec<RawEvent>> {
    let path = &ctx.file;
    let database = open_database(path)?;
    let mut statement = database
        .prepare(PARTS)
        .map_err(|error| database_error(path, "its parts", error))?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>("part_data")?,
                row.get::<_, String>("session_id")?,
                row.get::<_, i64>("created")?,
                row.get::<_, String>("message_data")?,
                row.get::<_, String>("directory")?,
            ))
        })
        .map_err(|error| database_error(path, "its parts", error))?;
    let mut events = Vec::new();
    for row in rows {
        let (part, session, created, message, directory) =
            row.map_err(|error| database_error(path, "a part", error))?;
        let (Ok(part), Ok(message)) = (
            serde_json::from_str::<Value>(&part),
            serde_json::from_str::<Value>(&message),
        ) else {
            continue;
        };
        let mut found = part_events(ctx, &part, &message, iso_millis(created));
        for event in &mut found {
            event.session_id = Some(session.clone());
            event.project = Some(directory.clone());
        }
        events.extend(found);
    }
    Ok(events)
}

/// The events one part holds, spoken by the role of the message it is in.
pub(crate) fn part_events(
    ctx: &ParserCtx,
    part: &Value,
    message: &Value,
    ts: Option<String>,
) -> Vec<RawEvent> {
    let speaker = match message.get("role").and_then(Value::as_str) {
        Some("assistant") => "assistant",
        _ => "user",
    };
    match part.get("type").and_then(Value::as_str) {
        Some("text") => {
            let synthetic = part.get("synthetic").and_then(Value::as_bool) == Some(true);
            match text_field(part, "text") {
                Some(text) if synthetic => vec![meta(ctx, ts, "synthetic_text", text, Map::new())],
                Some(text) => vec![event(ctx, ts, speaker, text)],
                None => Vec::new(),
            }
        }
        Some("reasoning") => text_field(part, "text")
            .map(|text| event(ctx, ts, "thinking", text))
            .into_iter()
            .collect(),
        Some("tool") => {
            let name = part.get("tool").and_then(Value::as_str);
            let id = part.get("callID").and_then(Value::as_str);
            let state = part.get("state");
            let field = |key: &str| state.and_then(|state| state.get(key));
            let mut events = vec![tool_call(
                ctx,
                ts.clone(),
                name,
                as_text(field("input")),
                id,
            )];
            match field("status").and_then(Value::as_str) {
                Some("completed") => events.push(tool_result(
                    ctx,
                    ts,
                    name,
                    as_text(field("output")),
                    id,
                    Some(false),
                )),
                Some("error") => events.push(tool_result(
                    ctx,
                    ts,
                    name,
                    as_text(field("error")),
                    id,
                    Some(true),
                )),
                _ => {}
            }
            events
        }
        Some("step-finish") => {
            let tokens = part.get("tokens");
            let count = |key: &str| tokens.and_then(|tokens| int_field(tokens, key));
            vec![usage(
                ctx,
                ts,
                text_field(message, "modelID"),
                count("input"),
                count("output"),
            )]
        }
        Some(other) => vec![meta(ctx, ts, other, String::new(), Map::new())],
        None => Vec::new(),
    }
}
