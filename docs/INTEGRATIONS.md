# Integration contracts

Transcript Lake's core is the local masking, partition, cursor, status, and query model defined in [CORE.md](CORE.md). Integrations translate external formats at narrow boundaries and may fail without invalidating unrelated core state.

The Transcript Lake maintainers own these contracts. Source-runtime maintainers own their external formats. Oko and Tama maintainers own their respective consumer and producer boundaries.

## Capability declaration

| Integration | Capability | Status | Required for core | Credentials | Supported boundary |
|---|---|---|---|---|---|
| Claude Code | Read local conversations and usage | Supported | No individual adapter is mandatory | None | Observed macOS JSONL project layout |
| Codex | Read local rollouts, reasoning, tools, and usage | Supported | No individual adapter is mandatory | None | Observed macOS rollout JSONL layout |
| OMP | Read top-level harness sessions | Supported | No individual adapter is mandatory | None | Observed OMP session JSONL layout; nested agent artifacts excluded |
| Droid | Read sessions and settings metadata | Supported | No individual adapter is mandatory | None | Observed Factory session JSONL and sidecar layout |
| Kimi Code | Read wire messages, tools, and turn usage | Supported | No individual adapter is mandatory | None | Observed Kimi wire and session-index layout |
| Tama adaptive hooks | Import validated closed decision segments; legacy log fallback | Supported | No | None | Segment protocol `hooks-telemetry-segment-v1`; legacy logs only when segment handoff directory is absent |
| DuckDB | SQL views and Parquet compaction | Supported optional tool | No | None | CLI `1.5.x` on `PATH` |
| Oko | Consume canonical per-session projection and optionally reindex | Supported optional tool | No | None for local invocation | Projection schema `oko-import-v1`; compatible `oko-cli transcripts reindex` |
| Gemini and Qwen | Transcript streaming | Not supported | No | None | Planned only after verified local formats exist |

Unsupported runtime selection fails before state mutation. The absence of one adapter root means that runtime contributes no files; it does not disable other runtimes.

## Runtime adapter boundary

Every `src/adapters/<runtime>.rs` module implements the `Adapter` trait:

- `runtime()`: stable runtime identifier;
- `roots(home)`: watched local roots;
- `list_sessions(root)`: historical replay entries;
- `entry_for(root, path)`: one notified source file without sibling enumeration;
- `parser(context)`: per-file `Parser` with `on_line` and `end`.

Adapters translate external records into canonical product intent. They do not write Lake state, perform network requests, fetch referenced tool-result files, or mask text themselves. `on_line` performs no I/O. The core driver applies bounds, recursive masking, partition ownership, cursors, and failure reporting.

Provider-specific record types, IDs, and usage fields remain inside adapter modules and bounded `extra` metadata. Shared core code dispatches at one adapter registry boundary rather than branching throughout workflows.

### Data crossing the boundary

- **Input:** local UTF-8 JSONL and documented sidecars owned by the runtime.
- **Output:** in-memory canonical events with ISO timestamps, runtime-native identity, event type, text, tool/model, usage, and bounded extra metadata.
- **Ordering:** source line order per file; cross-file ordering is not guaranteed.
- **Pagination:** not applicable; files stream from newline-aligned cursors.
- **Size:** text and nested metadata are bounded by core policy; full referenced result files are never followed.
- **Deduplication:** source byte cursors prevent append replay. Truncation or non-append replacement is rejected rather than guessed.
- **Retention:** vendor-owned; Lake retains only its masked copy according to operator policy.
- **Sensitivity:** raw transcript text is untrusted and potentially secret-bearing. It crosses only the in-process adapter-to-masker boundary.

A malformed source line is dropped by the adapter. A parser exception, unreadable file, invalid adapter, or invalid source transition leaves that file cursor unchanged and is logged by the stream; unrelated paths continue.

### Compatibility and removal

Support is pinned to the observed formats documented in [LAKE.md](LAKE.md), not to an unverified vendor marketing version. A format change is detected through parsing failures, missing required envelope fields, or qualification against a verified source sample. It must not be silently reinterpreted as another provider.

The live stream follows every supported runtime with an existing root; it has no mutable source-selection configuration. `rebuild --source <runtime>` narrows explicit historical recovery. Removing an adapter is a compatibility-breaking release change and never deletes the runtime's source files; existing Lake events retain their stable runtime identifier.

## Tama adaptive-hook integration

### Outcome and ownership

Tama can hand immutable, checksummed hook-decision segments to Transcript Lake. Tama owns segment production and the ready directory; Transcript Lake validates, claims, materializes, commits, and acknowledges each segment.

### Protocol and integrity

A closed segment declares protocol, segment ID, producer and invocation identity, creation time, ordered event frames, event count, and payload digest. Transcript Lake validates:

- regular-file and non-symlink shape;
- stable inode and size across the read;
- UTF-8 and final newline;
- open and close frames;
- exact sequence order and count;
- payload SHA-256;
- producer identity and filename agreement.

Invalid segments are not acknowledged and the stream logs the integration failure without blocking other sources. Valid output files are content checked; a conflicting existing output is a hard failure for that segment. Cursor commit precedes acknowledgement, so retry either completes the same transaction or republishes the existing acknowledgement.

### Selection and duplicate prevention

`HOOKS_ADAPTIVE_SEGMENTS_READY` optionally selects the ready directory. The default is the Tama local handoff path. When that directory exists, closed segments are the only hook source. Legacy mutable telemetry logs are watched only when the segment handoff directory is absent. The two paths are never streamed together, preventing double counting during migration.

No credential or network service is required. Removing the integration means stopping segment production and removing the stream's read access to the ready directory. Already committed hook events remain ordinary Lake evidence.

## DuckDB integration

DuckDB extends stable optional capabilities:

- bounded `sessions`, `events`, `stats`, and `hooks` commands;
- named Oko/Lake `signals` reports;
- arbitrary operator SQL over pinned local views;
- additive, runtime-filterable Parquet mirrors.

Configuration is executable discovery through `PATH`; there is no endpoint, credential, retry, or silent alternate engine. Supported compatibility is DuckDB CLI `1.5.x`. Missing binary, SQL error, extension error, or output conflict returns non-zero and preserves authoritative NDJSON.

Core views require no network. `signals` loads the compiled-in `signals.sql`, which may install DuckDB's SQLite extension and therefore may need network access on a fresh installation. It attaches the local Oko index read-only and selects one named report. Arbitrary `query` SQL is not sandboxed and may have DuckDB-defined write effects.

Removal is omission of analytics/compaction workflows and, after readers stop, `clean --target parquet --apply`. Streaming, masking, cursors, discovery, health, status, and Oko projection remain usable without DuckDB.

## Oko integration

### Outcome and capability

Transcript Lake materializes masked events as:

```text
LAKE_DATA/exports/oko/runtime=<runtime>/<session-hash>.jsonl
```

Each row declares `lake_schema: oko-import-v1`, deterministic event UUID, timestamp, runtime, native session identity, project, event type, text, tool, model, token counters, and bounded extra metadata. Oko recursively imports these files and owns its SQLite index.

The projection is derived and rebuildable. Live streaming merges newly canonicalized events into only the affected session files, preserves unchanged mtimes, and writes session files atomically before advancing the source cursor. Full export is an explicit recovery operation that rebuilds through staging and prunes only after a completed Lake scan.

### Configuration and failure

- Live projection needs no Oko installation and writes only derived files under `LAKE_DATA`.
- `rebuild-oko` reconstructs the projection from Lake partitions; `--reindex` then invokes `OKO_CLI` or `oko-cli` with `transcripts reindex --json`.
- `oko-refresh` invokes the compatible reindex command without reconstruction.

If Oko is unavailable, live projection and export recovery remain successful. An explicitly requested reindex that cannot start or exits non-zero makes that CLI operation exit non-zero and preserves the projection. Diagnostic prose goes to stderr; structured export stdout remains parseable.

No credential is exchanged. Transcript Lake does not write Oko's database directly. Oko is responsible for its own index locking, compatibility, retention, and UI behavior.

### Disable and remove

Stop invoking reindex, remove Oko's reference to the export, then delete `LAKE_DATA/exports/oko` if the derived files are no longer needed. Do not delete NDJSON partitions or cursors. Re-enabling rebuilds the export from authoritative Lake evidence.

## Reliability and diagnostics

Integrations do not use hidden unbounded retries. Local file operations either complete or preserve prior authoritative state; a failed source delta leaves its cursor unchanged and is retried on a later notification or process restart. Optional integration outage cannot prevent help, version, status, or unrelated source streaming.

Operator evidence includes stream lifecycle and per-source commit/failure lines, masking counts, partition inventory, Oko projection state, reindex status, and DuckDB exit status. Diagnostics may include local paths and process identity but never print unmasked transcript payloads or credentials.
