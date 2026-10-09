//! Canonical events and adapter contracts. Adapters emit unmasked text; the
//! stream owns the single masking boundary and adapters never write directly.
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::{Map, Value};

pub const HOOKS: &str = "hooks";

/// Every source name a command accepts: each registered transcript adapter's
/// runtime, in discovery order, then the adaptive-hook telemetry. The adapter
/// registry is the one list; nothing else names the runtimes.
pub fn supported_sources() -> Vec<&'static str> {
    let mut names: Vec<&'static str> = crate::adapters::all()
        .iter()
        .map(|adapter| adapter.runtime())
        .collect();
    names.push(HOOKS);
    names
}

/// The canonical event types, in the order the Lake contract lists them.
pub const EVENT_TYPES: [&str; 7] = [
    "user",
    "assistant",
    "thinking",
    "tool_call",
    "tool_result",
    "meta",
    "hook_decision",
];

/// One transcript file exposed by an adapter.
#[derive(Debug, Clone)]
pub struct SessionEntry {
    pub file: PathBuf,
    pub session_id: Option<String>,
    pub project: Option<String>,
}

/// What a parser learns about the file it is about to read.
#[derive(Debug, Clone)]
pub struct ParserCtx {
    pub file: PathBuf,
    pub session_id: Option<String>,
    pub project: Option<String>,
}

/// An event as an adapter produces it: unmasked, uncapped, runtime-agnostic.
#[derive(Debug, Clone, Default)]
pub struct RawEvent {
    pub ts: Option<String>,
    pub session_id: Option<String>,
    pub project: Option<String>,
    pub event_type: String,
    pub text: String,
    pub tool_name: Option<String>,
    pub model: Option<String>,
    pub tokens_in: Option<i64>,
    pub tokens_out: Option<i64>,
    pub extra: Map<String, Value>,
}

/// An event as it is persisted: masked, capped, and serialized in this exact
/// field order, which is the order every existing NDJSON partition uses.
#[derive(Debug, Serialize)]
pub struct CanonicalEvent {
    pub ts: Option<String>,
    pub runtime: String,
    pub machine: String,
    pub session_id: Option<String>,
    pub project: Option<String>,
    pub event_type: String,
    pub text: String,
    pub tool_name: Option<String>,
    pub model: Option<String>,
    pub tokens_in: Option<i64>,
    pub tokens_out: Option<i64>,
    pub extra: Map<String, Value>,
}

/// Line-driven translation of one vendor transcript file.
///
/// `on_line` must tolerate a malformed line by returning no events rather than
/// failing: a partial trailing line is normal while a runtime is still writing.
pub trait Parser {
    fn on_line(&mut self, line: &str) -> Vec<RawEvent>;
    /// Events the parser was holding when the file ended (open tool calls,
    /// buffered assistant turns). Called once, after the last line.
    fn end(&mut self) -> Vec<RawEvent> {
        Vec::new()
    }
}

/// Reads every event of a session kept whole, when the stream asks for it.
pub type WholeLoader = Box<dyn FnOnce() -> crate::util::Result<Vec<RawEvent>>>;

/// How one session is read.
pub enum Reading {
    /// Appended newline-delimited records, read from the stream's byte cursor.
    Lines(Box<dyn Parser>),
    /// The whole session read at once: a JSON document rewritten in place, a
    /// SQLite database (with its `-wal` file), a markdown log. The stream
    /// calls the loader only when the source's bytes changed since its last
    /// checkpoint, and recognises events it archived before by occurrence,
    /// so nothing is appended twice. A database that holds many sessions is
    /// one entry: the loader answers every session's events, each carrying
    /// its own `session_id`.
    Whole(WholeLoader),
}

/// One supported local transcript store.
pub trait Adapter {
    fn runtime(&self) -> &'static str;
    /// Existing source roots for this runtime under the given home directory.
    fn roots(&self, home: &Path) -> Vec<PathBuf>;
    /// The stable directories the stream watches for this runtime. A harness
    /// that keeps one root per project lists those projects in `roots`; a
    /// project that appears after the stream starts is only seen when its
    /// parent is watched, so such an adapter answers that parent here.
    fn watch_roots(&self, home: &Path) -> Vec<PathBuf> {
        self.roots(home)
    }
    /// Candidate transcript files beneath one root. A directory that vanishes
    /// mid-scan yields no entries instead of failing the run.
    fn list_sessions(&self, root: &Path) -> Vec<SessionEntry>;
    /// The entry for one transcript file whose path is already known, derived
    /// without listing a root. `None` when this adapter does not own the path
    /// or the path is not a transcript file it would have listed. A database
    /// adapter maps its `-wal` file to the database's own entry, because a
    /// SQLite write lands there first.
    ///
    /// This is what lets the online watcher read the file it was told about
    /// instead of re-walking every root to find it again.
    fn entry_for(&self, path: &Path) -> Option<SessionEntry>;
    /// How `ctx`'s session is read. No file is opened here: a line reader is
    /// handed the bytes by the stream, and a whole reader opens its source
    /// only when the stream calls its loader.
    fn read(&self, ctx: ParserCtx) -> Reading;
}

/// One partition file a segment producer published.
#[derive(Debug, Clone)]
pub struct SegmentOutput {
    pub path: std::path::PathBuf,
    pub sha256: String,
}

/// Where a producer that is not a plain line adapter hands its events. The
/// stream implements this boundary so masking, capping, canonicalization, and
/// partition placement stay in exactly one place.
///
/// Closed hook segments are immutable, so their output is published durably
/// (temp, fsync, rename, directory fsync) and identified by content: the
/// returned digests are what the caller commits to the cursor store, and what
/// makes re-publishing a segment with differing content a refusal rather than
/// an overwrite.
pub trait EventSink {
    /// Persist these events as belonging to `source_file`, which selects the
    /// partition file names, one per event date, returned in date order.
    fn accept(
        &mut self,
        source_file: &std::path::Path,
        events: &[RawEvent],
    ) -> crate::util::Result<Vec<SegmentOutput>>;
}
