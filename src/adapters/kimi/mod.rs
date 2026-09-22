//! Adapter: Kimi Code CLI wire transcripts —
//!   `~/.kimi-code/sessions/wd_*/session_*/agents/main/wire.jsonl`
//!
//! Frozen interface: `runtime`, `roots(home)`, `list_sessions(root)`, `parser(ctx)`.
//! Adapters emit UNMASKED text (the stream masks) and never do IO in `on_line`.
//! Shapes verified against the largest live wire files on this machine:
//!   metadata {protocol_version, app_version, created_at(epoch ms)}
//!   config.update {profileName, systemPrompt}          -> meta only, prompt dropped
//!   context.append_message {message:{role, content:[{type:'text', text}],
//!     origin:{kind}}, time} — origin.kind 'user' is the human turn; 'injection'
//!     and 'background_task' are synthetic context and become meta events.
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

use crate::types::{Adapter, Parser, ParserCtx, SessionEntry};

const TEXT_CAP: usize = 65536;

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

    fn parser(&self, ctx: ParserCtx) -> Box<dyn Parser> {
        Box::new(KimiParser::new(ctx))
    }
}

/// A root or session directory may vanish between scan and read; that is not an
/// error state worth failing the stream over. Other permissions or IO failures
/// were fatal in the previous implementation.
/// The frozen Rust signature reports them on stderr with the stream prefix.
/// Names come back in byte order, because Node's `readdirSync` sorts with strcmp.
fn read_dirents(dir: &Path) -> Vec<(String, fs::FileType)> {
    let iter = match fs::read_dir(dir) {
        Ok(iter) => iter,
        Err(error) => {
            // ENOENT and ENOTDIR: the tree moved under us.
            if !matches!(error.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory) {
                eprintln!(
                    "stream: listSessions failed under {}: {error}",
                    dir.display()
                );
            }
            return Vec::new();
        }
    };
    let mut out = Vec::new();
    for entry in iter.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        out.push((entry.file_name().to_string_lossy().to_string(), file_type));
    }
    out.sort_unstable_by(|left, right| left.0.cmp(&right.0));
    out
}

/// The type `read_dirents` would have reported for a path the caller already knows,
/// taken without following a symlink so that an entry the scan skips is skipped here
/// too. A path that vanished between notification and read has no type at all.
fn dirent_type(path: &Path) -> Option<fs::FileType> {
    fs::symlink_metadata(path).ok().map(|meta| meta.file_type())
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
    let Ok(text) = fs::read_to_string(parent.join("session_index.jsonl")) else {
        return map;
    };
    for line in text.split('\n') {
        if line.trim().is_empty() {
            continue;
        }
        // A torn index line only costs a project attribution, never the session.
        let Ok(rec) = serde_json::from_str::<Value>(line) else {
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

/// `String.prototype.slice(0, 65536)`, which counts UTF-16 code units. A cut that would
/// land inside a surrogate pair stops before it rather than emitting a lone surrogate.
pub(super) fn cap(value: &str) -> String {
    let mut units = 0usize;
    for (index, character) in value.char_indices() {
        let width = character.len_utf16();
        if units + width > TEXT_CAP {
            return value[..index].to_string();
        }
        units += width;
    }
    value.to_string()
}

/// A JSON string field, when present and non-empty.
pub(super) fn text_field(value: &Value, key: &str) -> Option<String> {
    match value.get(key) {
        Some(Value::String(text)) if !text.is_empty() => Some(text.clone()),
        _ => None,
    }
}

/// A finite JSON number field, defaulting to zero exactly as `num(x) || 0` did.
pub(super) fn num_or_zero(value: &Value, key: &str) -> i64 {
    value
        .get(key)
        .and_then(Value::as_f64)
        .filter(|raw| raw.is_finite())
        .map_or(0, |raw| raw as i64)
}

/// Wire times are epoch milliseconds; an out-of-range value must not kill the line.
pub(super) fn iso_from(value: Option<&Value>) -> Option<String> {
    let millis = value?.as_f64().filter(|raw| raw.is_finite())?;
    let stamp = chrono::DateTime::from_timestamp_millis(millis as i64)?;
    Some(stamp.format("%Y-%m-%dT%H:%M:%S%.3fZ").to_string())
}

mod records;

use records::KimiParser;
