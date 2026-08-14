# Changelog

All user-visible changes are recorded here. Transcript Lake uses Semantic Versioning. While the major version is zero, compatibility-breaking changes advance the minor version; additive and corrective changes advance the patch version and are distinguished in these notes.

## Unreleased

### Added

- Public product contract, release identity, onboarding, operational documentation, integration contracts, and canonical examples.
- Public CLI now covers path discovery, source discovery, health checks, safe rebuild, bounded sessions/events/statistics/hooks, Oko/Lake signals, structured output, filtered compaction, and preview-first derived cleanup.
- `transcript-lake search <text>` runs a bounded, newest-first, case-insensitive literal substring match over masked event text, with optional runtime, session, and type filters, so common text lookup no longer requires operator SQL. LIKE wildcards in the term are escaped and always match literally.
- `transcript-lake label add|list|aspects` records operator-owned session labels as aspect/value pairs (latest assignment per session and aspect wins in reads) in an append-only store beneath `LAKE_DATA/labels`, exposed to SQL through the canonical `labels` DuckDB view. Label writes do not take the events writer lease and never block the live stream.
- `transcript-lake label add --source <manual|model>` records label provenance; the flag stays `manual` when absent, so model-assisted suggestions (for example from transcript-label-trainer, carrying confidence in `note`) are no longer misrecorded as manual.
- Label provenance is now namespaced: `--source` accepts `manual`, `human`, `model`, or `brama`, each with an optional `:detail` suffix (for example `brama:claude-opus-4.6` or `model:hf-distilbert-topic`). `manual`/`human`/`brama:*` count as training ground truth; `model:*` marks a classifier suggestion awaiting acceptance and is excluded from training by convention (the filter lives in transcript-label-trainer).
- `transcript-lake stream [--json]` is the primary product path: one foreground process watches supported source roots, reads only the file named by each notification from its newline-aligned cursor, masks and appends canonical events, updates affected Oko sessions, and then checkpoints the source. It performs no periodic full scan and launches no refresh subprocess.
- `transcript-lake sessions --interrupted` lists conversations that stopped without an answer, across every streamed runtime, newest first. The canonical `interrupted_sessions` DuckDB view backs it: a session qualifies when its last recorded turn is a user message the agent never replied to (`stopped_as = 'unanswered'`) or a tool call cut off before the agent spoke again (`stopped_as = 'cut_off_mid_tool'`), and each row carries `last_user_text`, the masked opening of that final request.
- `transcript-lake show <session-id>` reconstructs one conversation from the Lake: oldest turn first, full masked event text with no per-event truncation, preceded by session identity and span and closed by a `rendered N of M` footer so a `--limit` cut is never silent. `--include` selects event types (`user,assistant` by default, `all` for the complete record including `thinking`, `tool_call`, `tool_result`, `meta`, and `hook_decision`), which makes an interrupted conversation found through `sessions --interrupted` readable in full instead of through truncated `events` rows or operator SQL.
- The masker has a fourth class, `credential`, which masks on syntax instead of entropy: `echo "<value>" | sudo -S`, an `expect` script answering a password prompt with `send "<value>\r"`, `--password <value>` and `-p<value>` for the tools that spell it that way, a JSON or object key named after a secret, and a `{"type":"password","val":"<value>"}` form-fill descriptor. An ordinary human password is invisible to the token, entropy, and assignment classes, which is how an operating-system password reached partitions in clear text. Only the value is replaced, so the command that carried it stays readable, and `maskCounts.credential` is reported with every commit alongside the existing classes.
- `scripts/scrub-known-secret.py` removes a literal that was already committed, which masking cannot reach: preview by default, `--apply` to write, the masker's own `[masked:credential:…]` spelling, the events writer lease held while it rewrites, every changed line re-parsed as JSON before the file is replaced, no other byte touched, and idempotent re-runs. The literal is read from a file or standard input and only its length and fingerprint are printed.
- `scripts/check-masker.py` compiles the tree and replays a credential fixture through `rebuild` twice, so masking rules and their idempotency are verified where an agent sandbox cannot build.
- The omp adapter now ingests subagent transcripts: `~/.omp/agent/sessions/<encoded-cwd>/<stamp>_<id>/<AgentName>.jsonl` and the deeper `<Agent>/<Agent>.<Child>.jsonl` files a delegating subagent writes, bounded to eight directory levels. Only the top-level session file was discovered before, so every delegated conversation — the majority of the work on a machine that fans out to agents — was missing from the archive while the session it belonged to looked complete. Each file opens with its own `session` record, so a subagent becomes its own session with its own identifier, cwd, partition, and Oko projection; artifacts sharing the session directory (`*.bash.log`, `*.md`, `local/`, `url-search/`, `*.jsonl.tombstone`) are still ignored. Existing Lakes backfill on the next catch-up: measured on this machine, 415 MB of real transcripts replay in 15 seconds into 0.43 bytes of partition plus Oko projection per source byte.

### Changed

- Transcript Lake is now implemented in Rust and ships as one self-contained binary. Existing canonical NDJSON partitions, `cursors.json`, `labels/*.ndjson`, `parquet/`, and Oko session files remain readable without migration; `stream-status.json` replaces the retired batch-run summary.
- Installation is now a Rust build: `cargo install --path .`, or `cargo build --release` and the binary at `target/release/transcript-lake`. Release artifacts are per-architecture macOS binary archives produced by `scripts/build-release.sh`.
- The canonical DuckDB view definitions are compiled into the binary instead of read from `sql/` at run time, so an installed CLI can never be separated from its views.
- The event-driven stream uses short single-writer transactions, fail-closed cursor validation, and explicit rejection of truncated or rewritten sources.
- Oko consumes Transcript Lake's canonical per-session projection for historical search instead of independently parsing vendor stores.
- Live Oko projection is part of the source transaction and reads no partitions; `rebuild-oko` is the reconstruction path.
- Tama closed segments take precedence over legacy mutable hook logs, so migration cannot double count the same decisions.
- Stream commits, projection reconstruction, Parquet compaction, and applied derived cleanup share the state writer lease.
- Common analytics use named CLI commands while arbitrary SQL remains available through `query`.

### Fixed

- Oko reindex now performs an uncapped first pass, distinguishes nanosecond mtime changes, and reparses truncated files.
- Oko token telemetry, goals, stats, and transcript rendering now consume normalized Lake rows without discarding provider identity or token usage.
- Explicit Oko reindex requests return non-zero status instead of presenting degraded work as success.
- Closed Tama segments now use the same real-time masking and projection path as transcript files, with content-matched acknowledgements and cursor-safe retry.
- Session files already open when `stream` starts now receive direct vnode watches, so long-lived OMP and Jeden conversations continue reaching their Oko projection without waiting for a restart catch-up.

### Removed

- Removed the specialized Droid-only Oko bridge and duplicate vendor-store indexing paths.
- Node.js is no longer required for anything, and the npm packaging path (`npm install --global .`, `npm uninstall --global`) is gone. DuckDB `1.5.x` remains the same optional external dependency for SQL queries and Parquet compaction.
- The Node and Python release helpers are gone: `scripts/build-release.mjs` and `scripts/surface.py` are replaced by `scripts/build-release.sh` and `scripts/surface.sh`, which produce the same artifacts and the same surface JSON.
- The batch `ingest` command, debounced `watch` command, `last-ingest.json`, hourly refresh wrapper, and timer LaunchAgents are removed. `stream` plus a KeepAlive supervisor is the only online path.

### Security

- Recursive metadata masking now covers nested strings and fails closed at the documented nesting bound.
- Oko export refuses malformed Lake rows before advancing its cursor or pruning prior derived sessions.

### Configuration and data migrations

- Oko historical indexing now expects canonical session files beneath `LAKE_DATA/exports/oko`; existing vendor transcripts remain available only for live operational launch and resume.
- Tama producers using `hooks-telemetry-segment-v1` should expose their ready directory at the default path or through `HOOKS_ADAPTIVE_SEGMENTS_READY`.
- Global `--data-dir <path>` selects a state root for one invocation and takes precedence over `LAKE_DATA`.
- Existing authoritative data needs no migration. The first stream start replaces obsolete diagnostic `last-ingest.json` with `stream-status.json`; canonical partitions and cursors are unchanged.
- `TRANSCRIPT_LAKE_SQL` optionally replaces the compiled-in view definitions with a directory of scripts, for iterating on views against an installed binary. A directory that is set but missing the requested script is an error, never a silent fall back.

### Operator actions

- Install and load the single KeepAlive service with `scripts/install-stream-service.sh`; it removes the retired refresh/watch LaunchAgents.
- Preserve any failed Lake and rebuild into a separate empty `LAKE_DATA` root after cursor damage or a non-append source change.
- Reinstall the CLI from this revision before loading the stream service.

### Known limitations

- No immutable release has been published yet.
- Current source formats are qualified only on macOS.

## Release-note requirements

Every release section must contain the headings above. Entries describe user impact rather than commit titles and state compatibility requirements, migrations, required operator actions, and known limitations. Empty categories remain present as `None` so a reader can distinguish reviewed absence from omission.
