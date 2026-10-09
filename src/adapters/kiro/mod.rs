//! Adapter: Kiro (kiro.dev) — the CLI's and the IDE's session files:
//!   CLI: `~/.kiro/sessions/cli/<session id>.jsonl`, beside the header
//!     `<session id>.json` (`{session_id, cwd, ...}`); each line is
//!     `{version, kind: Prompt | AssistantMessage | ToolResults, data:
//!     {message_id, content: [{kind: text | toolUse, data}], meta: {timestamp
//!     (seconds)}}}`, and one reply arrives as several AssistantMessage
//!     records sharing a `message_id`, each with the next piece.
//!   IDE and `kiro-cli --v3`: `~/.kiro/sessions/<workspace>/sess_<uuid>/
//!     messages.jsonl` beside `session.json` (`{workspacePaths, ...}`); each
//!     line is `{timestamp, payload: {type, content, ...}}` with type user |
//!     assistant (`operationType: Reasoning` is encrypted thinking) |
//!     tool_call {toolName, args, status} | tool_result, plus bookkeeping
//!     (session_metadata, usage_summary, turn_end); older files hold flat
//!     `{role, content}` lines.
//! Source: the observed-format registry vshulcz/deja-vu `docs/registry/kiro.md`
//! and its fixtures (kiro-cli 2.22.0 / 2.28). The headless kiro-cli's SQLite
//! rows are read by the `kiro-cli` adapter.
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use super::common::{
    as_text, entry, event, existing_root, file_name, files_with_suffix, is_file, iso_seconds, meta,
    read_json, subdirectories, text_field, tool_call, tool_result,
};
use crate::types::{Adapter, Parser, ParserCtx, RawEvent, Reading, SessionEntry};

const CLI: &str = "cli";
const IDE_MESSAGES: &str = "messages.jsonl";

pub struct Kiro;

impl Adapter for Kiro {
    fn runtime(&self) -> &'static str {
        "kiro"
    }

    fn watch_roots(&self, home: &Path) -> Vec<PathBuf> {
        existing_root(home.join(".kiro").join("sessions"))
    }

    fn roots(&self, home: &Path) -> Vec<PathBuf> {
        existing_root(home.join(".kiro").join("sessions"))
            .iter()
            .flat_map(|base| subdirectories(base))
            .collect()
    }

    fn list_sessions(&self, root: &Path) -> Vec<SessionEntry> {
        if root.file_name().is_some_and(|name| name == CLI) {
            return files_with_suffix(root, ".jsonl")
                .iter()
                .filter_map(|file| cli_entry(file))
                .collect();
        }
        subdirectories(root)
            .iter()
            .filter_map(|session| ide_entry(session))
            .collect()
    }

    fn entry_for(&self, path: &Path) -> Option<SessionEntry> {
        let roots = self.roots(&crate::util::home_dir());
        let parent = path.parent()?;
        if parent.file_name()? == CLI && roots.iter().any(|root| root.as_path() == parent) {
            return cli_entry(path);
        }
        let workspace = parent.parent()?;
        if path.file_name()? == IDE_MESSAGES && roots.iter().any(|root| root.as_path() == workspace)
        {
            return ide_entry(parent);
        }
        None
    }

    fn read(&self, ctx: ParserCtx) -> Reading {
        Reading::Lines(Box::new(KiroParser { ctx, pending: None }))
    }
}

fn cli_entry(file: &Path) -> Option<SessionEntry> {
    if !is_file(file) {
        return None;
    }
    let id = file_name(file)?.strip_suffix(".jsonl")?.to_string();
    let header = read_json(&file.with_extension("json")).ok();
    let project = header.as_ref().and_then(|header| text_field(header, "cwd"));
    Some(entry(file.to_path_buf(), Some(id), project))
}

fn ide_entry(session: &Path) -> Option<SessionEntry> {
    let messages = session.join(IDE_MESSAGES);
    if !is_file(&messages) {
        return None;
    }
    let header = read_json(&session.join("session.json")).ok();
    let project = header
        .as_ref()
        .and_then(|header| header.get("workspacePaths"))
        .and_then(Value::as_array)
        .and_then(|paths| paths.first())
        .and_then(Value::as_str)
        .map(str::to_string);
    Some(entry(
        messages,
        Some(session.file_name()?.to_string_lossy().to_string()),
        project,
    ))
}

/// A reply the CLI is still streaming: its message id, time and pieces.
struct Pending {
    id: String,
    ts: Option<String>,
    text: String,
}

struct KiroParser {
    ctx: ParserCtx,
    pending: Option<Pending>,
}

impl KiroParser {
    fn flush(&mut self) -> Vec<RawEvent> {
        self.pending
            .take()
            .map(|reply| event(&self.ctx, reply.ts, "assistant", reply.text))
            .into_iter()
            .collect()
    }

    fn cli(&mut self, kind: &str, data: &Value) -> Vec<RawEvent> {
        let ts = data
            .get("meta")
            .and_then(|meta| meta.get("timestamp"))
            .and_then(Value::as_f64)
            .and_then(iso_seconds);
        let id = text_field(data, "message_id");
        let mut events = Vec::new();
        let mut words = String::new();
        for part in data
            .get("content")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            match part.get("kind").and_then(Value::as_str) {
                Some("text") => words.push_str(&as_text(part.get("data"))),
                Some("toolUse") => {
                    let call = part.get("data");
                    events.push(tool_call(
                        &self.ctx,
                        ts.clone(),
                        call.and_then(|call| call.get("name"))
                            .and_then(Value::as_str),
                        as_text(
                            call.and_then(|call| call.get("input").or_else(|| call.get("args"))),
                        ),
                        call.and_then(|call| call.get("id")).and_then(Value::as_str),
                    ));
                }
                _ => {}
            }
        }
        let mut out = Vec::new();
        match kind {
            "AssistantMessage" => {
                let same = self
                    .pending
                    .as_ref()
                    .is_some_and(|reply| Some(&reply.id) == id.as_ref());
                if !same {
                    out.extend(self.flush());
                }
                if let (true, Some(reply)) = (same, self.pending.as_mut()) {
                    reply.text.push_str(&words);
                } else if let Some(id) = id {
                    self.pending = Some(Pending {
                        id,
                        ts,
                        text: words,
                    });
                } else if !words.is_empty() {
                    out.push(event(&self.ctx, ts, "assistant", words));
                }
            }
            "Prompt" => {
                out.extend(self.flush());
                if !words.is_empty() {
                    out.push(event(&self.ctx, ts, "user", words));
                }
            }
            "ToolResults" => {
                out.extend(self.flush());
                out.push(tool_result(
                    &self.ctx,
                    ts,
                    None,
                    as_text(Some(data)),
                    None,
                    None,
                ));
            }
            other => out.push(meta(&self.ctx, ts, other, String::new(), Map::new())),
        }
        out.extend(events);
        out
    }

    fn ide(&self, record: &Value) -> Vec<RawEvent> {
        let ts = text_field(record, "timestamp");
        let payload = match record.get("payload") {
            Some(payload) => payload,
            None => record,
        };
        let kind = payload
            .get("type")
            .or_else(|| payload.get("role"))
            .and_then(Value::as_str);
        let words = as_text(payload.get("content"));
        match kind {
            Some("user") if !words.is_empty() => vec![event(&self.ctx, ts, "user", words)],
            Some("assistant")
                if text_field(payload, "operationType").as_deref() == Some("Reasoning") =>
            {
                Vec::new()
            }
            Some("assistant") if !words.is_empty() => {
                vec![event(&self.ctx, ts, "assistant", words)]
            }
            Some("tool_call") => match text_field(payload, "status").as_deref() {
                Some("completed") | Some("failed") => vec![tool_call(
                    &self.ctx,
                    ts,
                    payload.get("toolName").and_then(Value::as_str),
                    as_text(payload.get("args")),
                    payload.get("toolUseId").and_then(Value::as_str),
                )],
                _ => Vec::new(),
            },
            Some("tool_result") => vec![tool_result(&self.ctx, ts, None, words, None, None)],
            Some(other) => vec![meta(&self.ctx, ts, other, String::new(), Map::new())],
            None => Vec::new(),
        }
    }
}

impl Parser for KiroParser {
    fn on_line(&mut self, line: &str) -> Vec<RawEvent> {
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            return Vec::new();
        };
        match (
            record.get("kind").and_then(Value::as_str),
            record.get("data"),
        ) {
            (Some(kind), Some(data)) => {
                let kind = kind.to_string();
                let data = data.clone();
                self.cli(&kind, &data)
            }
            _ => self.ide(&record),
        }
    }

    fn end(&mut self) -> Vec<RawEvent> {
        self.flush()
    }
}
