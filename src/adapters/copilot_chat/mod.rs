//! Adapter: chat in VS Code-family editors (GitHub Copilot Chat and every
//! other chat participant) —
//!   `<editor user dir>/workspaceStorage/<workspace id>/chatSessions/<id>.json`
//!   (a whole document) or `<id>.jsonl` (a mutation log), with the
//!   workspace's folder in `workspaceStorage/<workspace id>/workspace.json`,
//!   and sessions of windows with no workspace in
//!   `<editor user dir>/globalStorage/emptyWindowChatSessions/`.
//!
//! Source: microsoft/vscode `src/vs/workbench/contrib/chat/common/model/`:
//! `chatSessionStore.ts` (the two locations and the `.json` / `.jsonl`
//! files), `chatModel.ts` (`ISerializableChatData3 {version, sessionId,
//! creationDate, requests: [{requestId, message: string | {text, parts},
//! response: [...], timestamp, modelId}]}`; a markdown response part is
//! persisted as `{value}`, other parts keep their `kind` — `thinking
//! {value}`, `toolInvocationSerialized {toolId, toolCallId,
//! invocationMessage, pastTenseMessage, isComplete, resultDetails}`) and
//! `objectMutationLog.ts` (one `{kind, k?, v?, i?}` entry per line; the
//! kinds are `EntryKind`: Initial, Set, Push, Delete, numbered in that
//! order from zero). A log is replayed to the session's current state, so
//! sessions are read whole. A workspace folder that is not a `file://` URI
//! (a remote workspace) names no local project.
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use super::common::{
    as_text, editor_user_dirs, entry, event, file_name, files_with_suffix, is_file, iso_millis,
    meta, read_json, read_json_lines, subdirectories, text_field, tool_call, tool_result,
};
use crate::types::{Adapter, ParserCtx, RawEvent, Reading, SessionEntry};
use crate::util::Result;

mod log;

const SESSIONS: &str = "chatSessions";
const EMPTY_WINDOW: &str = "emptyWindowChatSessions";

pub struct CopilotChat;

impl Adapter for CopilotChat {
    fn runtime(&self) -> &'static str {
        "copilot-chat"
    }

    fn watch_roots(&self, home: &Path) -> Vec<PathBuf> {
        let mut roots = Vec::new();
        for user in editor_user_dirs(home) {
            let workspaces = user.join("workspaceStorage");
            if workspaces.is_dir() {
                roots.push(workspaces);
            }
            let empty = user.join("globalStorage").join(EMPTY_WINDOW);
            if empty.is_dir() {
                roots.push(empty);
            }
        }
        roots
    }

    fn roots(&self, home: &Path) -> Vec<PathBuf> {
        let mut roots = Vec::new();
        for user in editor_user_dirs(home) {
            for workspace in subdirectories(&user.join("workspaceStorage")) {
                let sessions = workspace.join(SESSIONS);
                if sessions.is_dir() {
                    roots.push(sessions);
                }
            }
            let empty = user.join("globalStorage").join(EMPTY_WINDOW);
            if empty.is_dir() {
                roots.push(empty);
            }
        }
        roots
    }

    fn list_sessions(&self, root: &Path) -> Vec<SessionEntry> {
        let project = workspace_folder(root);
        let mut sessions = Vec::new();
        for suffix in [".json", ".jsonl"] {
            for file in files_with_suffix(root, suffix) {
                if let Some(session) = session_entry(&file, project.clone()) {
                    sessions.push(session);
                }
            }
        }
        sessions
    }

    fn entry_for(&self, path: &Path) -> Option<SessionEntry> {
        let root = path.parent()?;
        if !self
            .roots(&crate::util::home_dir())
            .iter()
            .any(|known| known.as_path() == root)
            || !is_file(path)
        {
            return None;
        }
        session_entry(path, workspace_folder(root))
    }

    fn read(&self, ctx: ParserCtx) -> Reading {
        Reading::Whole(Box::new(move || load(&ctx)))
    }
}

/// The local folder a workspace's `workspace.json` names.
fn workspace_folder(sessions: &Path) -> Option<String> {
    let workspace = read_json(&sessions.parent()?.join("workspace.json")).ok()?;
    Some(
        text_field(&workspace, "folder")?
            .strip_prefix("file://")?
            .to_string(),
    )
}

fn session_entry(file: &Path, project: Option<String>) -> Option<SessionEntry> {
    let name = file_name(file)?;
    let id = name
        .strip_suffix(".jsonl")
        .or_else(|| name.strip_suffix(".json"))?;
    Some(entry(file.to_path_buf(), Some(id.to_string()), project))
}

fn load(ctx: &ParserCtx) -> Result<Vec<RawEvent>> {
    let session = if ctx
        .file
        .extension()
        .is_some_and(|extension| extension == "jsonl")
    {
        log::replay(&read_json_lines(&ctx.file)?)
    } else {
        read_json(&ctx.file)?
    };
    let mut events = Vec::new();
    for request in session
        .get("requests")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        events.extend(request_events(ctx, request));
    }
    Ok(events)
}

/// The person's message of one request, then the response's parts.
fn request_events(ctx: &ParserCtx, request: &Value) -> Vec<RawEvent> {
    let ts = request
        .get("timestamp")
        .and_then(Value::as_i64)
        .and_then(iso_millis);
    let mut events = Vec::new();
    let words = match request.get("message") {
        Some(Value::String(text)) => text.clone(),
        Some(message) => as_text(message.get("text")),
        None => String::new(),
    };
    if !words.is_empty() {
        events.push(event(ctx, ts.clone(), "user", words));
    }
    let mut answer: Vec<String> = Vec::new();
    for part in request
        .get("response")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        match part.get("kind").and_then(Value::as_str) {
            None => answer.extend(text_field(part, "value")),
            Some("thinking") => events.extend(
                text_field(part, "value")
                    .map(|thought| event(ctx, ts.clone(), "thinking", thought)),
            ),
            Some("toolInvocationSerialized") => {
                let name = part.get("toolId").and_then(Value::as_str);
                let id = part.get("toolCallId").and_then(Value::as_str);
                let message = part.get("invocationMessage");
                let said = match message.and_then(|message| message.get("value")) {
                    Some(value) => as_text(Some(value)),
                    None => as_text(message),
                };
                events.push(tool_call(ctx, ts.clone(), name, said, id));
                if let Some(details) = part
                    .get("resultDetails")
                    .filter(|details| !details.is_null())
                {
                    events.push(tool_result(
                        ctx,
                        ts.clone(),
                        name,
                        as_text(Some(details)),
                        id,
                        None,
                    ));
                }
            }
            Some(other) => events.push(meta(ctx, ts.clone(), other, String::new(), Map::new())),
        }
    }
    if !answer.is_empty() {
        let mut reply = event(ctx, ts, "assistant", answer.concat());
        reply.model = text_field(request, "modelId");
        events.push(reply);
    }
    events
}
