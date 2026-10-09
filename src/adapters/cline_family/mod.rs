//! Adapters: Cline and the extensions built from it, Roo Code and Kilo Code —
//!   `<editor user dir>/globalStorage/<extension id>/tasks/<task id>/
//!   api_conversation_history.json`, beside `ui_messages.json`, in every VS
//!   Code-family editor that installed the extension; the Cline CLI keeps the
//!   same task layout under `~/.cline/data/tasks`.
//!
//! Source: cline/cline `src/core/storage/disk.ts` (`GlobalFileNames`:
//! apiConversationHistory, uiMessages, taskMetadata; `tasks/<id>` under the
//! extension's global storage) and `src/shared/ExtensionMessage.ts`
//! (`ClineMessage {ts, type: ask|say, say?, ask?, text?}`); Roo Code
//! (RooCodeInc/Roo-Code `src/core/task-persistence/`) and Kilo Code
//! (Kilo-Org/kilocode) keep the same files.
//!   api_conversation_history.json: Anthropic `MessageParam[]` (`{role,
//!     content: string | [text | image | tool_use | tool_result | thinking]}`),
//!     each optionally carrying an epoch-millisecond `ts`; rewritten whole on
//!     every turn, so a task is read whole.
//!   ui_messages.json: the `say: "api_req_started"` entries carry the
//!     request's usage as JSON text `{tokensIn, tokensOut, cacheWrites,
//!     cacheReads, cost}`, which becomes a usage event.
//!   state/taskHistory.json (`HistoryItem {id, ts, task, cwdOnTaskInitialization}`)
//!     names the project a task ran in.
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::common::{
    anthropic_blocks, editor_user_dirs, entry, existing_root, int_field, is_file, iso_millis,
    message_time, read_json, subdirectories, text_field, usage,
};
use crate::types::{Adapter, ParserCtx, RawEvent, Reading, SessionEntry};
use crate::util::Result;

const HISTORY: &str = "api_conversation_history.json";
const UI_MESSAGES: &str = "ui_messages.json";

/// One Cline-family harness: its runtime id, its extension's identifier in
/// the editor's global storage, and where its command-line form keeps tasks.
pub struct ClineFamily {
    runtime: &'static str,
    extension: &'static str,
    cli_tasks: Option<&'static [&'static str]>,
}

/// Cline: extension `saoudrizwan.claude-dev`, CLI tasks in `~/.cline/data/tasks`.
pub const CLINE: ClineFamily = ClineFamily {
    runtime: "cline",
    extension: "saoudrizwan.claude-dev",
    cli_tasks: Some(&[".cline", "data", "tasks"]),
};
/// Roo Code: extension `rooveterinaryinc.roo-cline`.
pub const ROO: ClineFamily = ClineFamily {
    runtime: "roo",
    extension: "rooveterinaryinc.roo-cline",
    cli_tasks: None,
};
/// Kilo Code: extension `kilocode.kilo-code`.
pub const KILOCODE: ClineFamily = ClineFamily {
    runtime: "kilocode",
    extension: "kilocode.kilo-code",
    cli_tasks: None,
};

impl Adapter for ClineFamily {
    fn runtime(&self) -> &'static str {
        self.runtime
    }

    fn roots(&self, home: &Path) -> Vec<PathBuf> {
        let mut roots: Vec<PathBuf> = editor_user_dirs(home)
            .into_iter()
            .map(|user| {
                user.join("globalStorage")
                    .join(self.extension)
                    .join("tasks")
            })
            .filter(|tasks| tasks.is_dir())
            .collect();
        if let Some(parts) = self.cli_tasks {
            roots.extend(existing_root(
                parts
                    .iter()
                    .fold(home.to_path_buf(), |path, part| path.join(part)),
            ));
        }
        roots
    }

    fn list_sessions(&self, root: &Path) -> Vec<SessionEntry> {
        let projects = task_projects(root);
        subdirectories(root)
            .into_iter()
            .filter_map(|task| task_entry(&task, &projects))
            .collect()
    }

    fn entry_for(&self, path: &Path) -> Option<SessionEntry> {
        let task = path.parent()?;
        let root = task.parent()?;
        if !self
            .roots(&crate::util::home_dir())
            .iter()
            .any(|known| known.as_path() == root)
        {
            return None;
        }
        task_entry(task, &task_projects(root))
    }

    fn read(&self, ctx: ParserCtx) -> Reading {
        Reading::Whole(Box::new(move || load(&ctx)))
    }
}

/// Each task's project, from the extension's task history beside `tasks`.
fn task_projects(tasks: &Path) -> HashMap<String, String> {
    let Some(storage) = tasks.parent() else {
        return HashMap::new();
    };
    let Ok(history) = read_json(&storage.join("state").join("taskHistory.json")) else {
        return HashMap::new();
    };
    history
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| {
            Some((
                text_field(item, "id")?,
                text_field(item, "cwdOnTaskInitialization")?,
            ))
        })
        .collect()
}

fn task_entry(task: &Path, projects: &HashMap<String, String>) -> Option<SessionEntry> {
    let history = task.join(HISTORY);
    if !is_file(&history) {
        return None;
    }
    let id = task.file_name()?.to_string_lossy().to_string();
    let project = projects.get(&id).cloned();
    Some(entry(history, Some(id), project))
}

fn load(ctx: &ParserCtx) -> Result<Vec<RawEvent>> {
    let history = read_json(&ctx.file)?;
    let mut events = Vec::new();
    for message in history.as_array().into_iter().flatten() {
        let role = message.get("role").and_then(Value::as_str);
        if let Some(role) = role {
            events.extend(anthropic_blocks(
                ctx,
                role,
                message.get("content"),
                message_time(message),
            ));
        }
    }
    let Some(task) = ctx.file.parent() else {
        return Ok(events);
    };
    let ui = task.join(UI_MESSAGES);
    if is_file(&ui) {
        for message in read_json(&ui)?.as_array().into_iter().flatten() {
            if text_field(message, "say").as_deref() != Some("api_req_started") {
                continue;
            }
            let Some(request) = text_field(message, "text")
                .and_then(|text| serde_json::from_str::<Value>(&text).ok())
            else {
                continue;
            };
            let at = message
                .get("ts")
                .and_then(Value::as_i64)
                .and_then(iso_millis);
            events.push(usage(
                ctx,
                at,
                None,
                int_field(&request, "tokensIn"),
                int_field(&request, "tokensOut"),
            ));
        }
    }
    Ok(events)
}
