//! Adapter: Amp (Sourcegraph) — threads kept locally:
//!   `~/.local/share/amp/threads/<thread id>.json`, one JSON document per
//!   thread, rewritten as it grows.
//!
//! Source: the observed-format registry vshulcz/deja-vu `docs/registry/amp.md`
//! and its fixture. Amp builds from 0.0.1774963753 on keep threads on
//! ampcode.com and write no thread files; this reads the files earlier builds
//! wrote and those a plugin writes out. A thread is `{id, title, created
//! (epoch ms), env: {initial: {trees: [{uri: file://...}]}}, messages: [{role,
//! content: [text | tool_use | tool_result | thinking blocks], meta: {sentAt}?,
//! usage: {timestamp, model, inputTokens, outputTokens}?}]}`.
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::common::{
    anthropic_blocks, entry, existing_root, file_name, files_with_suffix, int_field, is_file,
    iso_millis, iso_millis_field, read_json, text_field, usage,
};
use crate::types::{Adapter, ParserCtx, RawEvent, Reading, SessionEntry};
use crate::util::Result;

pub struct Amp;

impl Adapter for Amp {
    fn runtime(&self) -> &'static str {
        "amp"
    }

    fn roots(&self, home: &Path) -> Vec<PathBuf> {
        existing_root(
            home.join(".local")
                .join("share")
                .join("amp")
                .join("threads"),
        )
    }

    fn list_sessions(&self, root: &Path) -> Vec<SessionEntry> {
        files_with_suffix(root, ".json")
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
        Reading::Whole(Box::new(move || load(&ctx)))
    }
}

fn session_entry(file: &Path) -> Option<SessionEntry> {
    Some(entry(
        file.to_path_buf(),
        Some(file_name(file)?.strip_suffix(".json")?.to_string()),
        None,
    ))
}

fn load(ctx: &ParserCtx) -> Result<Vec<RawEvent>> {
    let thread = read_json(&ctx.file)?;
    let project = thread
        .pointer("/env/initial/trees/0/uri")
        .and_then(Value::as_str)
        .and_then(|uri| uri.strip_prefix("file://"))
        .map(str::to_string);
    let mut at = iso_millis_field(&thread, "created");
    let mut events = Vec::new();
    for message in thread
        .get("messages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(role) = text_field(message, "role") else {
            continue;
        };
        let spent = message.get("usage");
        if let Some(sent) = message
            .get("meta")
            .and_then(|meta| int_field(meta, "sentAt"))
            .and_then(iso_millis)
        {
            at = Some(sent);
        } else if let Some(stamp) = spent.and_then(|spent| text_field(spent, "timestamp")) {
            at = Some(stamp);
        }
        events.extend(anthropic_blocks(
            ctx,
            &role,
            message.get("content"),
            at.clone(),
        ));
        if let Some(spent) = spent {
            let count = |key: &str| int_field(spent, key);
            events.push(usage(
                ctx,
                at.clone(),
                text_field(spent, "model"),
                count("inputTokens"),
                count("outputTokens"),
            ));
        }
    }
    let session = text_field(&thread, "id");
    for event in &mut events {
        event.project = project.clone();
        if session.is_some() {
            event.session_id = session.clone();
        }
    }
    Ok(events)
}
