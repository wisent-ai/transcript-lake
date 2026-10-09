//! Adapter: Junie (JetBrains' coding agent, CLI and IDE) —
//!   `~/.junie/sessions/<session id>/events.jsonl`, one event per line
//!   `{kind, timestampMs, ...}`, with `sessions/index.jsonl` listing each
//!   session's `projectDir`.
//!
//! Source: the observed-format registry vshulcz/deja-vu `docs/registry/junie.md`
//! and its fixture (Junie CLI 3110.7):
//!   UserPromptEvent {prompt}                       -> user
//!   SessionA2uxEvent {event: {agentEvent: {kind, stepId, ...}}} — the agent's
//!     work as blocks updated in place, so a step's last update is the one
//!     read and a session is read whole:
//!     TerminalBlockUpdatedEvent {command, output, exitCode, status}
//!                                                  -> tool_call + tool_result
//!     ViewFilesBlockUpdatedEvent {files: [{relativePath}]}  -> tool_call
//!     FileChangesBlockUpdatedEvent {changes: [{afterRelativePath}]} -> tool_call
//!     MarkdownBlockUpdatedEvent {text}, ResultBlockUpdatedEvent {result}
//!                                                  -> assistant
//!     LlmResponseMetadataEvent {modelUsage: [{model, inputTokens,
//!       outputTokens}]}                            -> usage
//!   every other kind                               -> nothing (task state)
use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::common::{
    as_text, entry, event, existing_root, int_field, is_file, iso_millis, read_json_lines,
    subdirectories, text_field, tool_call, tool_result, usage,
};
use crate::types::{Adapter, ParserCtx, RawEvent, Reading, SessionEntry};
use crate::util::Result;

const EVENTS: &str = "events.jsonl";

pub struct Junie;

impl Adapter for Junie {
    fn runtime(&self) -> &'static str {
        "junie"
    }

    fn roots(&self, home: &Path) -> Vec<PathBuf> {
        existing_root(home.join(".junie").join("sessions"))
    }

    fn list_sessions(&self, root: &Path) -> Vec<SessionEntry> {
        let projects = projects(root);
        subdirectories(root)
            .iter()
            .filter_map(|session| session_entry(session, &projects))
            .collect()
    }

    fn entry_for(&self, path: &Path) -> Option<SessionEntry> {
        let session = path.parent()?;
        let root = session.parent()?;
        if path.file_name()? != EVENTS
            || !self
                .roots(&crate::util::home_dir())
                .iter()
                .any(|known| known.as_path() == root)
        {
            return None;
        }
        session_entry(session, &projects(root))
    }

    fn read(&self, ctx: ParserCtx) -> Reading {
        Reading::Whole(Box::new(move || load(&ctx)))
    }
}

/// Each session's project, from the index beside the sessions.
fn projects(root: &Path) -> HashMap<String, String> {
    read_json_lines(&root.join("index.jsonl"))
        .into_iter()
        .flatten()
        .filter_map(|line| {
            Some((
                text_field(&line, "sessionId").or_else(|| text_field(&line, "id"))?,
                text_field(&line, "projectDir")?,
            ))
        })
        .collect()
}

fn session_entry(session: &Path, projects: &HashMap<String, String>) -> Option<SessionEntry> {
    let events = session.join(EVENTS);
    if !is_file(&events) {
        return None;
    }
    let id = session.file_name()?.to_string_lossy().to_string();
    let project = projects.get(&id).cloned();
    Some(entry(events, Some(id), project))
}

fn load(ctx: &ParserCtx) -> Result<Vec<RawEvent>> {
    let mut order: Vec<String> = Vec::new();
    let mut steps: HashMap<String, (Option<String>, Value)> = HashMap::new();
    let mut events = Vec::new();
    for record in read_json_lines(&ctx.file)? {
        let ts = int_field(&record, "timestampMs").and_then(iso_millis);
        match text_field(&record, "kind").as_deref() {
            Some("UserPromptEvent") => {
                events.extend(
                    text_field(&record, "prompt").map(|prompt| event(ctx, ts, "user", prompt)),
                );
            }
            Some("SessionA2uxEvent") => {
                let Some(block) = record
                    .get("event")
                    .and_then(|event| event.get("agentEvent"))
                else {
                    continue;
                };
                if let Some(step) = text_field(block, "stepId") {
                    if !steps.contains_key(&step) {
                        order.push(step.clone());
                    }
                    steps.insert(step, (ts, block.clone()));
                } else if text_field(block, "kind").as_deref() == Some("LlmResponseMetadataEvent") {
                    for spent in block
                        .get("modelUsage")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                    {
                        let count = |key: &str| int_field(spent, key);
                        events.push(usage(
                            ctx,
                            ts.clone(),
                            text_field(spent, "model"),
                            count("inputTokens"),
                            count("outputTokens"),
                        ));
                    }
                }
            }
            _ => {}
        }
    }
    for step in order {
        if let Some((ts, block)) = steps.remove(&step) {
            events.extend(block_events(ctx, &step, &block, ts));
        }
    }
    Ok(events)
}

fn block_events(ctx: &ParserCtx, step: &str, block: &Value, ts: Option<String>) -> Vec<RawEvent> {
    match text_field(block, "kind").as_deref() {
        Some("TerminalBlockUpdatedEvent") => {
            let failed = text_field(block, "status").map(|status| status == "FAILED");
            vec![
                tool_call(
                    ctx,
                    ts.clone(),
                    Some("terminal"),
                    as_text(block.get("command")),
                    Some(step),
                ),
                tool_result(
                    ctx,
                    ts,
                    Some("terminal"),
                    as_text(block.get("output")),
                    Some(step),
                    failed,
                ),
            ]
        }
        Some("ViewFilesBlockUpdatedEvent") => {
            let files = block
                .get("files")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|file| text_field(file, "relativePath"));
            vec![tool_call(
                ctx,
                ts,
                Some("view_files"),
                files.collect::<Vec<_>>().join("\n"),
                Some(step),
            )]
        }
        Some("FileChangesBlockUpdatedEvent") => {
            let files = block
                .get("changes")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|change| text_field(change, "afterRelativePath"));
            vec![tool_call(
                ctx,
                ts,
                Some("file_changes"),
                files.collect::<Vec<_>>().join("\n"),
                Some(step),
            )]
        }
        Some("MarkdownBlockUpdatedEvent") => text_field(block, "text")
            .map(|text| event(ctx, ts, "assistant", text))
            .into_iter()
            .collect(),
        Some("ResultBlockUpdatedEvent") => text_field(block, "result")
            .map(|text| event(ctx, ts, "assistant", text))
            .into_iter()
            .collect(),
        _ => Vec::new(),
    }
}
