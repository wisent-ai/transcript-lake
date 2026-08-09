# Changelog

All user-visible changes are recorded here. Transcript Lake uses Semantic Versioning. While the major version is zero, compatibility-breaking changes advance the minor version; additive and corrective changes advance the patch version and are distinguished in these notes.

## Unreleased

### Added

- Public product contract, release identity, onboarding, operational documentation, integration contracts, and canonical examples.
- Public CLI now covers path discovery, source discovery, health checks, safe rebuild, bounded sessions/events/statistics/hooks, Oko/Lake signals, structured output, filtered compaction, and preview-first derived cleanup.
- `transcript-lake search <text>` runs a bounded, newest-first, case-insensitive literal substring match over masked event text, with optional runtime, session, and type filters, so common text lookup no longer requires operator SQL. LIKE wildcards in the term are escaped and always match literally.
- `transcript-lake label add|list|aspects` records operator-owned session labels as aspect/value pairs (latest assignment per session and aspect wins in reads) in an append-only store beneath `LAKE_DATA/labels`, exposed to SQL through the canonical `labels` DuckDB view. Label writes do not take the events writer lease and never block a concurrent ingest.
- `transcript-lake label add --source <manual|model>` records label provenance; the flag stays `manual` when absent, so model-assisted suggestions (for example from transcript-label-trainer, carrying confidence in `note`) are no longer misrecorded as manual.
- Label provenance is now namespaced: `--source` accepts `manual`, `human`, `model`, or `brama`, each with an optional `:detail` suffix (for example `brama:claude-opus-4.6` or `model:hf-distilbert-topic`). `manual`/`human`/`brama:*` count as training ground truth; `model:*` marks a classifier suggestion awaiting acceptance and is excluded from training by convention (the filter lives in transcript-label-trainer).
- `transcript-lake watch [--debounce <seconds>] [--json]` keeps the Lake near-real-time: a long-running foreground process recursively watches every supported source root, coalesces changes over a quiet interval, and runs the same ingest-then-export refresh as the external timer (at most one run in flight and one queued, with the writer lease as the backstop). launchd or systemd is expected to KeepAlive it; the scheduled timer remains the backstop.
- `transcript-lake sessions --interrupted` lists conversations that stopped without an answer, across every ingested runtime, newest first. The canonical `interrupted_sessions` DuckDB view backs it: a session qualifies when its last recorded turn is a user message the agent never replied to (`stopped_as = 'unanswered'`) or a tool call cut off before the agent spoke again (`stopped_as = 'cut_off_mid_tool'`), and each row carries `last_user_text`, the masked opening of that final request.
- `transcript-lake show <session-id>` reconstructs one conversation from the Lake: oldest turn first, full masked event text with no per-event truncation, preceded by session identity and span and closed by a `rendered N of M` footer so a `--limit` cut is never silent. `--include` selects event types (`user,assistant` by default, `all` for the complete record including `thinking`, `tool_call`, `tool_result`, `meta`, and `hook_decision`), which makes an interrupted conversation found through `sessions --interrupted` readable in full instead of through truncated `events` rows or operator SQL.

### Changed

- Transcript Lake is now implemented in Rust and ships as one self-contained binary. The CLI surface is unchanged — same commands, flags, human output, JSON keys, error sentences, and exit codes — and so is every persisted format: NDJSON partition lines keep their exact field order, `cursors.json`, `labels/*.ndjson`, `last-ingest.json`, `parquet/`, and the Oko export keep their paths and shapes. An existing `LAKE_DATA` is read and appended to by the new binary with no migration.
- Installation is now a Rust build: `cargo install --path .`, or `cargo build --release` and the binary at `target/release/transcript-lake`. Release artifacts are per-architecture macOS binary archives produced by `scripts/build-release.sh`.
- The canonical DuckDB view definitions are compiled into the binary instead of read from `sql/` at run time, so an installed CLI can never be separated from its views.
- Ingest now uses a single-writer lease, fail-closed cursor validation, and explicit rejection of truncated or rewritten sources.
- Oko now imports Transcript Lake's canonical per-session export for historical search instead of independently parsing the same vendor stores.
- Oko export now covers every supported runtime, performs safe incremental tail reads, and rebuilds through staging when source partitions are replaced.
- Tama closed segments take precedence over legacy mutable hook logs, so migration cannot double count the same decisions.
- Ingest, Oko export, Parquet compaction, and applied derived cleanup now share the state writer lease.
- Common analytics use named CLI commands while arbitrary SQL remains available through `query`.

### Fixed

- Oko reindex now performs an uncapped first pass, distinguishes nanosecond mtime changes, and reparses truncated files.
- Oko token telemetry, goals, stats, and transcript rendering now consume normalized Lake rows without discarding provider identity or token usage.
- Explicit Oko reindex requests and partial ingest now return non-zero status instead of presenting degraded work as success.
- Ingesting closed hook segments now reports the masking hits it actually produced. The previous implementation skipped the accounting assignment on the segment branch, so `perRuntime.hooks.maskedHits` in the ingest summary and in `last-ingest.json` was always `0` and every recorded summary understated it. Only the counter was wrong; the events themselves were always masked.

### Removed

- Removed the specialized Droid-only Oko bridge and duplicate vendor-store indexing paths.
- Node.js is no longer required for anything, and the npm packaging path (`npm install --global .`, `npm uninstall --global`) is gone. DuckDB `1.5.x` remains the same optional external dependency for SQL queries and Parquet compaction.
- The Node and Python release helpers are gone: `scripts/build-release.mjs` and `scripts/surface.py` are replaced by `scripts/build-release.sh` and `scripts/surface.sh`, which produce the same artifacts and the same surface JSON.

### Security

- Recursive metadata masking now covers nested strings and fails closed at the documented nesting bound.
- Oko export refuses malformed Lake rows before advancing its cursor or pruning prior derived sessions.

### Configuration and data migrations

- Oko historical indexing now expects canonical session files beneath `LAKE_DATA/exports/oko`; existing vendor transcripts remain available only for live operational launch and resume.
- Tama producers using `hooks-telemetry-segment-v1` should expose their ready directory at the default path or through `HOOKS_ADAPTIVE_SEGMENTS_READY`.
- Global `--data-dir <path>` selects a state root for one invocation and takes precedence over `LAKE_DATA`.
- No data migration is required by the Rust rewrite; it changes no persisted format.
- `TRANSCRIPT_LAKE_SQL` optionally replaces the compiled-in view definitions with a directory of scripts, for iterating on views against an installed binary. A directory that is set but missing the requested script is an error, never a silent fall back.

### Operator actions

- Back up the current Lake, run a full Oko export, and run `oko-cli transcripts reindex` after adopting this development revision.
- Preserve any failed Lake and rebuild into a separate empty `LAKE_DATA` root after cursor damage or a non-append source change.
- Reinstall the CLI with `cargo install --path .` and remove any previously installed global npm package. Update schedulers and LaunchAgents so their `PATH` reaches the new binary (`~/.cargo/bin` by default).

### Known limitations

- No immutable release has been published yet.
- Current source formats are qualified only on macOS.

## Release-note requirements

Every release section must contain the headings above. Entries describe user impact rather than commit titles and state compatibility requirements, migrations, required operator actions, and known limitations. Empty categories remain present as `None` so a reader can distinguish reviewed absence from omission.
