//! Adapter: aider —
//!   `.aider.chat.history.md`, appended Markdown in the directory aider runs
//!   in (the git root). aider keeps no list of the projects it ran in, so the
//!   history read is the one a launch from the home directory writes,
//!   `~/.aider.chat.history.md`; the home is not watched recursively, so it is
//!   read when the stream starts.
//!
//! Source: aider's `aider/io.py` (`chat_history_file`, `# aider chat started
//! at`, `#### ` user lines, `> ` tool output) and the observed-format
//! registry vshulcz/deja-vu `docs/registry/aider.md` (aider 0.86.2). One file
//! holds many launches, each opened by `# aider chat started at YYYY-MM-DD
//! HH:MM:SS` (local time). Outside fenced code, `#### ` lines are the
//! person's words, `> ` lines and the lines under them are aider's own output,
//! and the rest is the model's reply. aider records no time per message, so
//! every message takes its launch's time; a launch's session id is
//! `aider-<launch time>`.
use std::path::{Path, PathBuf};

use chrono::TimeZone;

use super::common::{entry, event, file_name, is_file, tool_result};
use crate::types::{Adapter, ParserCtx, RawEvent, Reading, SessionEntry};
use crate::util::{Error, Result};

const HISTORY: &str = ".aider.chat.history.md";
const STARTED: &str = "# aider chat started at ";
const USER: &str = "#### ";
const OUTPUT: &str = "> ";
const FENCE: &str = "```";

pub struct Aider;

impl Adapter for Aider {
    fn runtime(&self) -> &'static str {
        "aider"
    }

    fn roots(&self, home: &Path) -> Vec<PathBuf> {
        if is_file(&home.join(HISTORY)) {
            vec![home.to_path_buf()]
        } else {
            Vec::new()
        }
    }

    /// The home directory is not watched: the history is read when the
    /// stream starts and whenever a source is adopted.
    fn watch_roots(&self, _home: &Path) -> Vec<PathBuf> {
        Vec::new()
    }

    fn list_sessions(&self, root: &Path) -> Vec<SessionEntry> {
        let history = root.join(HISTORY);
        if is_file(&history) {
            vec![entry(
                history,
                None,
                Some(root.to_string_lossy().to_string()),
            )]
        } else {
            Vec::new()
        }
    }

    fn entry_for(&self, path: &Path) -> Option<SessionEntry> {
        if file_name(path)? != HISTORY || !is_file(path) {
            return None;
        }
        let root = path.parent()?;
        Some(entry(
            path.to_path_buf(),
            None,
            Some(root.to_string_lossy().to_string()),
        ))
    }

    fn read(&self, ctx: ParserCtx) -> Reading {
        Reading::Whole(Box::new(move || load(&ctx)))
    }
}

/// The launch time a header names, as ISO-8601 UTC.
fn launch_time(header: &str) -> Option<String> {
    let naive = chrono::NaiveDateTime::parse_from_str(header.trim(), "%Y-%m-%d %H:%M:%S").ok()?;
    let local = chrono::Local.from_local_datetime(&naive).single()?;
    Some(
        local
            .with_timezone(&chrono::Utc)
            .format("%Y-%m-%dT%H:%M:%S%.3fZ")
            .to_string(),
    )
}

/// What the lines gathered so far are.
#[derive(Clone, Copy, PartialEq)]
enum Speaker {
    Person,
    Model,
    Aider,
}

struct Launch<'a> {
    ctx: &'a ParserCtx,
    ts: Option<String>,
    session: String,
    events: Vec<RawEvent>,
    speaker: Option<Speaker>,
    lines: Vec<String>,
}

impl Launch<'_> {
    fn flush(&mut self) {
        let text = self.lines.join("\n").trim().to_string();
        self.lines.clear();
        let mut said = match (self.speaker, text.is_empty()) {
            (_, true) | (None, _) => return,
            (Some(Speaker::Person), _) => event(self.ctx, self.ts.clone(), "user", text),
            (Some(Speaker::Model), _) => event(self.ctx, self.ts.clone(), "assistant", text),
            (Some(Speaker::Aider), _) => {
                tool_result(self.ctx, self.ts.clone(), Some("aider"), text, None, None)
            }
        };
        said.session_id = Some(self.session.clone());
        self.events.push(said);
    }

    fn take(&mut self, speaker: Speaker, line: String) {
        if self.speaker != Some(speaker) {
            self.flush();
            self.speaker = Some(speaker);
        }
        self.lines.push(line);
    }
}

fn load(ctx: &ParserCtx) -> Result<Vec<RawEvent>> {
    let text = std::fs::read_to_string(&ctx.file).map_err(|error| {
        Error(format!(
            "cannot read the aider history {}: {error}",
            ctx.file.display()
        ))
    })?;
    let mut events = Vec::new();
    let mut launch: Option<Launch> = None;
    let mut fenced = false;
    for line in text.lines() {
        if let Some(header) = line.strip_prefix(STARTED) {
            if let Some(mut done) = launch.take() {
                done.flush();
                events.extend(done.events);
            }
            let ts = launch_time(header);
            let session = format!("aider-{}", header.trim());
            launch = Some(Launch {
                ctx,
                ts,
                session,
                events: Vec::new(),
                speaker: None,
                lines: Vec::new(),
            });
            fenced = false;
            continue;
        }
        let Some(current) = launch.as_mut() else {
            continue;
        };
        if line.trim_start().starts_with(FENCE) {
            fenced = !fenced;
        }
        if fenced || line.trim_start().starts_with(FENCE) {
            let speaker = match current.speaker {
                Some(speaker) => speaker,
                None => Speaker::Model,
            };
            current.take(speaker, line.to_string());
        } else if let Some(words) = line.strip_prefix(USER) {
            current.take(Speaker::Person, words.to_string());
        } else if let Some(output) = line.strip_prefix(OUTPUT) {
            current.take(Speaker::Aider, output.to_string());
        } else if current.speaker == Some(Speaker::Person) && line.is_empty() {
            current.lines.push(String::new());
        } else {
            current.take(Speaker::Model, line.to_string());
        }
    }
    if let Some(mut done) = launch {
        done.flush();
        events.extend(done.events);
    }
    Ok(events)
}
