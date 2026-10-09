//! Adapter: CodeWhale (formerly deepseek-tui) —
//!   `~/.codewhale/sessions/<id>.json` (before the rebrand
//!   `~/.deepseek/sessions/<id>.json`), one pretty-printed JSON document per
//!   session, rewritten as it grows: `{schema_version, metadata: {id, title,
//!   created_at, updated_at, workspace, parent_session_id?}, messages: [{role,
//!   content: Anthropic's blocks}]}`.
//!
//! Source: the observed-format registry vshulcz/deja-vu
//! `docs/registry/codewhale.md` (CodeWhale 0.9.6–0.10.1). Messages carry no
//! time of their own, so each takes the session's `created_at`. The `system`
//! and `developer` roles are CodeWhale's own framing (compaction and branch
//! summaries) and become tool-side text, not either side of the conversation.
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::common::{
    anthropic_blocks, entry, existing_root, file_name, files_with_suffix, is_file, read_json,
    text_field,
};
use crate::types::{Adapter, ParserCtx, RawEvent, Reading, SessionEntry};
use crate::util::Result;

pub struct CodeWhale;

impl Adapter for CodeWhale {
    fn runtime(&self) -> &'static str {
        "codewhale"
    }

    fn roots(&self, home: &Path) -> Vec<PathBuf> {
        let mut roots = existing_root(home.join(".codewhale").join("sessions"));
        roots.extend(existing_root(home.join(".deepseek").join("sessions")));
        roots
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
    let session = read_json(&ctx.file)?;
    let Some(metadata) = session.get("metadata") else {
        return Ok(Vec::new());
    };
    let started = text_field(metadata, "created_at");
    let project = text_field(metadata, "workspace");
    let mut events = Vec::new();
    for message in session
        .get("messages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if let Some(role) = text_field(message, "role") {
            events.extend(anthropic_blocks(
                ctx,
                &role,
                message.get("content"),
                started.clone(),
            ));
        }
    }
    for event in &mut events {
        event.project = project.clone();
    }
    Ok(events)
}
