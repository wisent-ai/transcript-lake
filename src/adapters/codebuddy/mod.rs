//! Adapter: CodeBuddy Code (Tencent's `codebuddy` CLI) and WorkBuddy, which
//! runs the same agent —
//!   `~/.codebuddy/projects/<mangled cwd>/<session id>.jsonl` (WorkBuddy:
//!   `~/.workbuddy/projects/...`, `~/.workbuddy-ai/projects/...`), appended one
//!   OpenAI Responses-style item per line, `timestamp` in epoch milliseconds
//!   and `cwd` on the record.
//!
//! Source: the observed-format registry vshulcz/deja-vu
//! `docs/registry/codebuddy.md` (checked against CodeBuddy 2.161.2):
//!   message {role, content: [{type: input_text | output_text, text}]}
//!                              -> user / assistant; a user item CodeBuddy's
//!                                 own `isRealUserMessageItem` rejects — one
//!                                 with `providerData.skipRun`, or flagged
//!                                 `isMeta`, `isCompactInternal`,
//!                                 `isCompacted`, `isSummary` or carrying
//!                                 `teammateMessage` — is the harness's own and
//!                                 becomes metadata
//!   function_call {callId, name, arguments}          -> tool_call
//!   function_call_result {callId, name, output}      -> tool_result
//!   reasoning {content | summary}                    -> thinking
//!   summary, ai-title, custom-title, topic, turn-metrics, session-meta, ...
//!                                                    -> meta by type
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use super::common::{
    as_text, entry, event, existing_root, file_name, files_with_suffix, int_field, is_file,
    iso_millis, meta, subdirectories, text_field, text_of, tool_call, tool_result,
};
use crate::types::{Adapter, Parser, ParserCtx, RawEvent, Reading, SessionEntry};

/// The homes the agent keeps its projects under.
const HOMES: &[&str] = &[".codebuddy", ".workbuddy", ".workbuddy-ai"];

pub struct CodeBuddy;

impl Adapter for CodeBuddy {
    fn runtime(&self) -> &'static str {
        "codebuddy"
    }

    fn watch_roots(&self, home: &Path) -> Vec<PathBuf> {
        HOMES
            .iter()
            .flat_map(|dir| existing_root(home.join(dir).join("projects")))
            .collect()
    }

    fn roots(&self, home: &Path) -> Vec<PathBuf> {
        HOMES
            .iter()
            .flat_map(|dir| existing_root(home.join(dir).join("projects")))
            .flat_map(|base| subdirectories(&base))
            .collect()
    }

    fn list_sessions(&self, root: &Path) -> Vec<SessionEntry> {
        files_with_suffix(root, ".jsonl")
            .iter()
            .filter_map(|file| session_entry(file))
            .collect()
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
        session_entry(path)
    }

    fn read(&self, ctx: ParserCtx) -> Reading {
        Reading::Lines(Box::new(CodeBuddyParser { ctx }))
    }
}

fn session_entry(file: &Path) -> Option<SessionEntry> {
    let id = file_name(file)?.strip_suffix(".jsonl")?.to_string();
    Some(entry(file.to_path_buf(), Some(id), None))
}

/// Whether the item carries `key` set to anything but false.
fn flagged(item: &Value, key: &str) -> bool {
    item.get(key)
        .is_some_and(|value| !value.is_null() && value != &Value::Bool(false))
}

/// Whether a user item is one the harness wrote, not the person.
fn written_by_harness(item: &Value) -> bool {
    item.get("providerData")
        .is_some_and(|data| flagged(data, "skipRun"))
        || flagged(item, "isMeta")
        || flagged(item, "isCompactInternal")
        || flagged(item, "isCompacted")
        || flagged(item, "isSummary")
        || flagged(item, "teammateMessage")
}

struct CodeBuddyParser {
    ctx: ParserCtx,
}

impl Parser for CodeBuddyParser {
    fn on_line(&mut self, line: &str) -> Vec<RawEvent> {
        let Ok(item) = serde_json::from_str::<Value>(line) else {
            return Vec::new();
        };
        if let Some(cwd) = text_field(&item, "cwd") {
            self.ctx.project = Some(cwd);
        }
        let ctx = &self.ctx;
        let ts = int_field(&item, "timestamp").and_then(iso_millis);
        let id = item.get("callId").and_then(Value::as_str);
        let name = item.get("name").and_then(Value::as_str);
        match text_field(&item, "type").as_deref() {
            Some("message") => {
                let words = text_of(item.get("content"));
                if words.is_empty() {
                    return Vec::new();
                }
                match text_field(&item, "role").as_deref() {
                    Some("user") if written_by_harness(&item) => {
                        vec![meta(ctx, ts, "harness_message", words, Map::new())]
                    }
                    Some("user") => vec![event(ctx, ts, "user", words)],
                    Some("assistant") => vec![event(ctx, ts, "assistant", words)],
                    _ => vec![meta(ctx, ts, "message", words, Map::new())],
                }
            }
            Some("function_call") => {
                vec![tool_call(ctx, ts, name, as_text(item.get("arguments")), id)]
            }
            Some("function_call_result") => {
                let output = item.get("output");
                let text = match output.and_then(|output| output.get("text")) {
                    Some(text) => as_text(Some(text)),
                    None => as_text(output),
                };
                vec![tool_result(ctx, ts, name, text, id, None)]
            }
            Some("reasoning") => {
                let thought = text_of(item.get("content").or_else(|| item.get("summary")));
                if thought.is_empty() {
                    Vec::new()
                } else {
                    vec![event(ctx, ts, "thinking", thought)]
                }
            }
            Some(other) => vec![meta(ctx, ts, other, String::new(), Map::new())],
            None => Vec::new(),
        }
    }
}
