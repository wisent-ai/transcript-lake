//! Adapter: Amazon Q Developer CLI (`q chat`) and its successor kiro-cli in
//! headless mode —
//!   `<local data dir>/amazon-q/data.sqlite3` (`~/Library/Application
//!   Support/amazon-q` on macOS, `~/.local/share/amazon-q` elsewhere), table
//!   `conversations (key, value)`: one row per working directory, its key the
//!   directory and its value the directory's last conversation as JSON;
//!   kiro-cli: `<local data dir>/kiro-cli/data.sqlite3`, table
//!   `conversations_v2 (key, conversation_id, value, created_at, updated_at)`,
//!   the same JSON (the observed-format registry vshulcz/deja-vu
//!   `docs/registry/kiro.md` and its `internal/sources/kiro_db.go`).
//!
//! Source: aws/amazon-q-developer-cli `crates/chat-cli/src/database/mod.rs`
//! (`Table::Conversations`, migration `007_conversations_table`) and
//! `crates/chat-cli/src/cli/chat/conversation.rs` / `message.rs`:
//! `ConversationState {conversation_id, history: [HistoryEntry {user,
//! assistant}], ...}`; `UserMessage {content, timestamp}` with content
//! `{"Prompt": {prompt}}`, `{"CancelledToolUses": {prompt, tool_use_results}}`
//! or `{"ToolUseResults": {tool_use_results: [{tool_use_id, content:
//! [{"Text": ...} | {"Json": ...}], status}]}}`; `AssistantMessage` is
//! `{"Response": {message_id, content}}` or `{"ToolUse": {message_id,
//! content, tool_uses: [{id, name, args}]}}`. A conversation row is replaced
//! as the conversation grows, so the database is read whole.
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::common::{
    as_text, database_error, database_of, entry, event, existing_root, is_file, open_database,
    text_field, tool_call, tool_result,
};
use crate::types::{Adapter, ParserCtx, RawEvent, Reading, SessionEntry};
use crate::util::Result;

const DATABASE: &str = "data.sqlite3";

/// One harness that keeps Amazon Q's conversation rows: its runtime id, its
/// data directory's name and the table.
pub struct QStore {
    runtime: &'static str,
    data_dir: &'static str,
    table: &'static str,
}

/// Amazon Q Developer CLI.
pub const AMAZON_Q: QStore = QStore {
    runtime: "amazon-q",
    data_dir: "amazon-q",
    table: "conversations",
};
/// kiro-cli's headless conversations.
pub const KIRO_CLI: QStore = QStore {
    runtime: "kiro-cli",
    data_dir: "kiro-cli",
    table: "conversations_v2",
};

impl Adapter for QStore {
    fn runtime(&self) -> &'static str {
        self.runtime
    }

    fn roots(&self, home: &Path) -> Vec<PathBuf> {
        let mut roots = existing_root(
            home.join("Library")
                .join("Application Support")
                .join(self.data_dir),
        );
        roots.extend(existing_root(
            home.join(".local").join("share").join(self.data_dir),
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
        let table = self.table;
        Reading::Whole(Box::new(move || load(&ctx, table)))
    }
}

fn load(ctx: &ParserCtx, table: &str) -> Result<Vec<RawEvent>> {
    let path = &ctx.file;
    let database = open_database(path)?;
    let mut statement = database
        .prepare(&format!("SELECT key, value FROM {table} ORDER BY key"))
        .map_err(|error| database_error(path, &format!("its {table}"), error))?;
    let rows = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>("key")?, row.get::<_, String>("value")?))
        })
        .map_err(|error| database_error(path, "its conversations", error))?;
    let mut events = Vec::new();
    for row in rows {
        let (directory, value) =
            row.map_err(|error| database_error(path, "a conversation", error))?;
        let Ok(state) = serde_json::from_str::<Value>(&value) else {
            continue;
        };
        let session = text_field(&state, "conversation_id");
        let mut found = Vec::new();
        for turn in state
            .get("history")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            found.extend(turn_events(ctx, turn));
        }
        for event in &mut found {
            event.session_id = session.clone();
            event.project = Some(directory.clone());
        }
        events.extend(found);
    }
    Ok(events)
}

/// The text of tool result blocks: `{"Text": ...}` or `{"Json": ...}`.
fn result_text(blocks: Option<&Value>) -> String {
    blocks
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|block| match (block.get("Text"), block.get("Json")) {
            (Some(text), _) => as_text(Some(text)),
            (None, json) => as_text(json),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn turn_events(ctx: &ParserCtx, turn: &Value) -> Vec<RawEvent> {
    let mut events = Vec::new();
    let user = turn.get("user");
    let ts = user.and_then(|user| text_field(user, "timestamp"));
    let content = user.and_then(|user| user.get("content"));
    if let Some(prompt) = content
        .and_then(|content| content.get("Prompt"))
        .and_then(|prompt| text_field(prompt, "prompt"))
    {
        events.push(event(ctx, ts.clone(), "user", prompt));
    }
    let results = content.and_then(|content| {
        content
            .get("ToolUseResults")
            .or_else(|| content.get("CancelledToolUses"))
    });
    for result in results
        .and_then(|results| results.get("tool_use_results"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let failed = result
            .get("status")
            .and_then(Value::as_str)
            .map(|status| status == "Error");
        events.push(tool_result(
            ctx,
            ts.clone(),
            None,
            result_text(result.get("content")),
            result.get("tool_use_id").and_then(Value::as_str),
            failed,
        ));
    }
    let assistant = turn.get("assistant");
    let answer = assistant.and_then(|assistant| {
        assistant
            .get("Response")
            .or_else(|| assistant.get("ToolUse"))
    });
    if let Some(words) = answer.and_then(|answer| text_field(answer, "content")) {
        events.push(event(ctx, ts.clone(), "assistant", words));
    }
    for tool in answer
        .and_then(|answer| answer.get("tool_uses"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        events.push(tool_call(
            ctx,
            ts.clone(),
            tool.get("name").and_then(Value::as_str),
            as_text(tool.get("args")),
            tool.get("id").and_then(Value::as_str),
        ));
    }
    events
}
