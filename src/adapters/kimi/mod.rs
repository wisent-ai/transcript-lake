//! Adapter: Kimi Code CLI wire transcripts —
//!   `~/.kimi-code/sessions/wd_*/session_*/agents/main/wire.jsonl`
//!
//! Interface: `runtime`, `roots(home)`, `list_sessions(root)`, `entry_for(path)`, `read(ctx)`.
//! Adapters emit UNMASKED text (the stream masks) and never do IO in `on_line`.
//! Supported wire shapes:
//!   metadata {protocol_version, app_version, created_at(epoch ms)}
//!   config.update {profileName, systemPrompt}          -> meta only, prompt dropped
//!   context.append_message {message:{role, content:[{type:'text', text}],
//!     origin:{kind}}, time} — origin.kind 'user' is the human turn and
//!     sets extra.attribution=user; an absent origin does not prove authorship.
//!     'injection' and 'background_task' are synthetic context and become meta.
//!   turn.prompt / turn.steer duplicate append_message one-to-one -> skipped.
//!   context.append_loop_event {event:{type}, time} carries the assistant loop:
//!     content.part {part:{type:'text'|'think'}}         (each part arrives complete)
//!     tool.call    {toolCallId, name, args, description}
//!     tool.result  {toolCallId, result:{output, isError?}}
//!     step.begin / step.end                             -> dropped (usage.record wins)
//!   usage.record {model, usage:{input*, output}, usageScope:'turn'|'session'} —
//!     'turn' records mirror step.end usage exactly (per-step deltas, summable);
//!     'session' records are cumulative snapshots and only refresh the model.
//!   context.apply_compaction {summary} and turn.cancel  -> small meta events.
//! Wire records carry no session id or cwd; the session id is the `session_*` dirname
//! and `~/.kimi-code/session_index.jsonl` maps sessionId -> workDir (the only place the
//! absolute project path exists, so `list_sessions` performs that one small read).
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::common::{appended_record, dirent_type, read_dirents};
use crate::types::{Adapter, ParserCtx, Reading, SessionEntry};

pub struct Kimi;

impl Adapter for Kimi {
    fn runtime(&self) -> &'static str {
        "kimi"
    }

    fn roots(&self, home: &Path) -> Vec<PathBuf> {
        let dir = home.join(".kimi-code").join("sessions");
        if dir.exists() {
            return vec![dir];
        }
        Vec::new()
    }

    fn list_sessions(&self, root: &Path) -> Vec<SessionEntry> {
        let index = read_work_dir_index(root);
        let mut sessions = Vec::new();
        for (name, file_type) in read_dirents(root) {
            if !file_type.is_dir() || !name.starts_with("wd_") {
                continue;
            }
            let work_dir = root.join(&name);
            for (entry, entry_type) in read_dirents(&work_dir) {
                if !entry_type.is_dir() {
                    continue;
                }
                if let Some(session) = session_entry(&work_dir, &entry, &index) {
                    sessions.push(session);
                }
            }
        }
        sessions
    }

    fn entry_for(&self, path: &Path) -> Option<SessionEntry> {
        // The wire transcript is the only file a scan ever offers, and it sits at a
        // fixed depth: `<root>/wd_*/session_*/agents/main/wire.jsonl`.
        if path.file_name()?.to_string_lossy() != "wire.jsonl" {
            return None;
        }
        let main = path.parent()?;
        let agents = main.parent()?;
        if main.file_name()?.to_string_lossy() != "main"
            || agents.file_name()?.to_string_lossy() != "agents"
        {
            return None;
        }
        let session_dir = agents.parent()?;
        let work_dir = session_dir.parent()?;
        let home = crate::util::home_dir();
        let root = self
            .roots(&home)
            .into_iter()
            .find(|root| work_dir.parent() == Some(root.as_path()))?;
        if !work_dir.file_name()?.to_string_lossy().starts_with("wd_")
            || !dirent_type(work_dir)?.is_dir()
            || !dirent_type(session_dir)?.is_dir()
        {
            return None;
        }
        let name = session_dir.file_name()?.to_string_lossy();
        // The work-dir index is the only place the absolute project path exists, so
        // resolving one file pays the same small read the scan pays once per root.
        session_entry(work_dir, &name, &read_work_dir_index(&root))
    }

    fn read(&self, ctx: ParserCtx) -> Reading {
        Reading::Lines(Box::new(KimiParser::new(ctx)))
    }
}

/// The entry a scan yields for one directory inside a work directory: `session_*` only,
/// and only once it holds a wire transcript, with the directory name as the session id
/// and the project the index records for it. Shared with `entry_for` so one known path
/// and a full scan cannot disagree about a file.
fn session_entry(
    work_dir: &Path,
    name: &str,
    index: &HashMap<String, String>,
) -> Option<SessionEntry> {
    if !name.starts_with("session_") {
        return None;
    }
    let file = work_dir
        .join(name)
        .join("agents")
        .join("main")
        .join("wire.jsonl");
    if !file.exists() {
        return None;
    }
    Some(SessionEntry {
        file,
        session_id: Some(name.to_string()),
        project: index.get(name).cloned(),
    })
}

fn read_work_dir_index(root: &Path) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let Some(parent) = root.parent() else {
        return map;
    };
    // No index is no attribution; an index that cannot be read costs every
    // session its project, so it is named, as an unlistable root is.
    let index = parent.join("session_index.jsonl");
    let text = match fs::read_to_string(&index) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return map,
        Err(error) => {
            eprintln!(
                "stream: session index {} cannot be read, so no session under it is attributed to a project: {error}",
                index.display()
            );
            return map;
        }
    };
    for line in text.split('\n') {
        // A torn index line only costs a project attribution, never the
        // session; it is named, as every line that is not JSON is.
        let Some(rec) = appended_record(&index, line) else {
            continue;
        };
        if let (Some(Value::String(session_id)), Some(Value::String(work_dir))) =
            (rec.get("sessionId"), rec.get("workDir"))
        {
            map.insert(session_id.clone(), work_dir.clone());
        }
    }
    map
}

mod records;

use records::KimiParser;
