# Changelog

All user-visible changes are recorded here. Transcript Lake uses Semantic Versioning. While the major version is zero, compatibility-breaking changes advance the minor version; additive and corrective changes advance the patch version and are distinguished in these notes.

## Unreleased

### Added

- Public product contract, release identity, onboarding, operational documentation, integration contracts, and canonical examples.
- Public CLI now covers path discovery, source discovery, health checks, safe rebuild, bounded sessions/events/statistics/hooks, Oko/Lake signals, structured output, filtered compaction, and preview-first derived cleanup.
- `transcript-lake search <text>` runs a bounded, newest-first, case-insensitive literal substring match over masked event text, with optional runtime, session, and type filters, so common text lookup no longer requires operator SQL. LIKE wildcards in the term are escaped and always match literally.

### Changed

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

### Removed

- Removed the specialized Droid-only Oko bridge and duplicate vendor-store indexing paths.

### Security

- Recursive metadata masking now covers nested strings and fails closed at the documented nesting bound.
- Oko export refuses malformed Lake rows before advancing its cursor or pruning prior derived sessions.

### Configuration and data migrations

- Oko historical indexing now expects canonical session files beneath `LAKE_DATA/exports/oko`; existing vendor transcripts remain available only for live operational launch and resume.
- Tama producers using `hooks-telemetry-segment-v1` should expose their ready directory at the default path or through `HOOKS_ADAPTIVE_SEGMENTS_READY`.
- Global `--data-dir <path>` selects a state root for one invocation and takes precedence over `LAKE_DATA`.

### Operator actions

- Back up the current Lake, run a full Oko export, and run `oko-cli transcripts reindex` after adopting this development revision.
- Preserve any failed Lake and rebuild into a separate empty `LAKE_DATA` root after cursor damage or a non-append source change.

### Known limitations

- No immutable release has been published yet.
- Current source formats are qualified only on macOS.

## Release-note requirements

Every release section must contain the headings above. Entries describe user impact rather than commit titles and state compatibility requirements, migrations, required operator actions, and known limitations. Empty categories remain present as `None` so a reader can distinguish reviewed absence from omission.
