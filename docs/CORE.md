# Core functionality contract

This document defines the smallest provider-neutral system that fulfills Transcript Lake's README promise. Integrations may consume or extend these contracts but cannot redefine them.

## Workflow: incrementally ingest local evidence

- **Actor:** local developer or scheduled operator process.
- **Initial state:** a selected writable `LAKE_DATA`; zero or more supported local source stores; no concurrent Lake writer.
- **Input:** optional runtime selector and deliberate full-mode flag.
- **Authoritative inputs:** vendor transcript stores and closed hook segments.
- **Authoritative Lake state:** masked NDJSON partitions plus the cursor store that proves consumed source boundaries.
- **Success:** structured summary, durable newline-complete events, and cursors no further than persisted events.
- **Partial completion:** successfully checkpointed files remain valid; failed files increment `failures`, set `partial`, and cause a non-zero CLI exit after the summary is emitted.
- **Retry:** incremental retry resumes from durable cursors and must not repeat checkpointed source bytes.
- **Recovery:** interruption is retryable. Corrupt cursor state, source truncation, and non-append rewrite are not replayed into an existing Lake; recovery targets a separate empty root.
- **Security:** raw text is masked before partition writes. No credential is required. Source stores are read-only.

Full mode is not an in-place destructive repair. It is a complete source replay allowed only when `LAKE_DATA` is empty. This turns replacement into an explicit operator-controlled cutover: build a new Lake, inspect it, stop writers, then choose which root to retain.

## Workflow: inspect state

`transcript-lake status` is read-only. It reports selected state root, runtime partition counts and bytes, cursor inventory and freshness, last-ingest masking summary, and Oko freshness when available.

Status never creates configuration, starts ingestion, repairs state, contacts providers, or claims that a missing optional integration invalidates core partitions. Human formatting is not a stable machine schema.

## Workflow: query evidence

`transcript-lake query "<sql>"` loads the frozen local DuckDB views over the selected Lake and executes the supplied SQL. The query process does not mutate NDJSON, cursors, or source stores. DuckDB is an explicit optional prerequisite; absence is an actionable dependency failure, not a fallback to another engine.

The canonical `events` view pins column names and types and tolerates only a torn final partition line during a concurrent read. Aggregate views expose sessions, tools, tokens, and hook decisions. User SQL can itself create external files or perform DuckDB mutations; the operator owns the supplied SQL. Transcript Lake does not label arbitrary SQL as read-only.

## Workflow: compact derived state

`transcript-lake compact` creates per-runtime Parquet mirrors under `LAKE_DATA/parquet`. NDJSON remains authoritative and is never deleted. Existing output conflicts or DuckDB failure stop that runtime and produce a non-zero exit. Retrying is safe only after the operator inspects or removes the conflicting derived artifact.

## Public command semantics

| Command | Mutation | Success | Failure |
|---|---|---|---|
| no command / `help` / `--help` | None | Actionable guidance on stdout | None |
| `--version` | None | Canonical `package.json` version | Invalid installation fails before a false version is printed |
| `ingest` | `LAKE_DATA` only | JSON summary; zero exit only when not partial | Actionable stderr, structured partial evidence when available, non-zero exit |
| `status` | None | Human inventory and freshness | Dependency-specific diagnostic without repair |
| `query` | User SQL may have DuckDB-defined effects | DuckDB result | Non-zero dependency or SQL error |
| `compact` | Derived Parquet only | Per-runtime size and path report | Non-zero; NDJSON preserved |
| `export-oko` | Derived Oko export only | JSON export summary | Non-zero; partitions and cursors preserved |
| `oko-refresh` | Oko-owned index through `oko-cli` | Child-process success | Non-zero or actionable missing-CLI guidance |

Unknown commands, flags, and contradictory input are rejected before mutation. `--source` accepts only the declared runtime set.

## Canonical event contract

Every durable event has:

- ISO timestamp or explicit unknown partition placement;
- normalized runtime and machine identity;
- runtime-native session identity and best-effort project path;
- event type;
- bounded masked text;
- optional tool and model names;
- normalized input and output usage counters;
- bounded, depth-limited, recursively masked extra metadata.

The detailed field table is frozen in [LAKE.md](LAKE.md). Provider adapters emit the same in-memory intent. They never write state or perform I/O while mapping one source line.

## Mutable state and ownership

| Resource | Authority | Writer | Concurrency | Recovery and retention |
|---|---|---|---|---|
| Vendor transcripts | Vendor runtime | Vendor runtime | External | Never changed by Lake |
| Closed hook segments | Tama hook runtime until handed off | Tama then Lake handoff | Segment protocol | Preserve segment evidence on failure |
| `events/` NDJSON | Transcript Lake | One ingest lease per root | Append-only | Backup or rebuild into a separate root |
| `cursors.json` | Transcript Lake | One locked read-modify-write transaction | Atomic merge under lock | Hard-fail on corruption; never silently reset |
| `last-ingest.json` | Transcript Lake CLI | Current ingest | Atomicity limited to one writer lease | Diagnostic; may be regenerated |
| `parquet/` | DuckDB compaction | Operator invocation | No concurrent compaction contract | Delete and rebuild from NDJSON |
| `exports/oko/` | Lake exporter | Export invocation | Atomic session/cursor writes | Delete and rebuild from NDJSON |
| Oko SQLite index | Oko | Oko | External single-writer contract | Rebuild from Lake export |

A writer lease fails fast when another live owner holds the root. Stale same-host claims are reclaimed only after process identity no longer matches. Shared cross-host mutation is unsupported.

## Failure classification

- **Invalid input:** unknown command, flag, or runtime; reject before mutation.
- **Missing optional dependency:** DuckDB or Oko unavailable; affected operation fails while unrelated core commands remain usable.
- **Permission failure:** name the source or state path; do not silently skip it as success.
- **Source conflict:** truncation or same-size rewrite; reject replay into existing partitions.
- **State conflict:** active writer; fail fast and preserve incumbent ownership.
- **Corrupt authoritative metadata:** cursor parse or numeric validation failure; preserve state and require separate-root recovery.
- **Partial source failure:** preserve completed checkpoints, report failure count, and exit non-zero.
- **Interruption:** append and cursor ordering permits incremental retry.
- **Storage exhaustion:** write fails; cursor cannot advance past a failed batch. Operator frees space or selects another root before retry.

There is no hidden retry loop. Operators or external schedulers choose when to retry after reading the classified result.

## Configuration

- `LAKE_DATA`: startup configuration selecting one absolute or relative state root; resolved to an absolute path. Default `~/.transcript-lake`.
- `OKO_CLI`: optional integration executable override. Default path discovery searches `PATH` only when the integration is invoked.
- `HOOKS_ADAPTIVE_SEGMENTS_READY`: optional Tama handoff location used only by the hook integration.

Unknown CLI flags are rejected. Environment variables not documented here are not part of the public product contract. Resolved paths appear in status without exposing transcript contents or credentials.

## Security and privacy boundary

Masking protects credential-shaped and high-entropy text before durable Lake writes, but it is not anonymization. Session identifiers, machine name, timestamps, project path, model, tool name, sizes, counts, and short non-matching text can remain sensitive. Protect `LAKE_DATA` with local filesystem permissions and do not publish it.

The masker is deterministic and idempotent. Nested metadata is depth bounded and text fields are size capped. Full tool-result files are referenced but never followed. No core command transmits transcript data over the network.

## Resource behavior

Ingest streams source files from newline-aligned offsets and batches a bounded number of events before checkpointing. Individual text fields are capped. Nested extra metadata is depth bounded. Session count and total Lake disk usage are not globally capped because they reflect operator-selected source history; operators control scope with `--source`, an empty target root, storage quotas, and scheduling.

The product supports one writer and concurrent read-only status or query operations per local state root. It defines no shared-filesystem, multi-host, or distributed-write topology.

## Evolution

Removal or incompatible change to commands, flags, runtime identifiers, configuration, event fields, cursor semantics, partition layout, masking guarantees, or integration export schemas follows [release policy](RELEASES.md). Stored-state changes require explicit migration and rollback analysis. Obsolete paths are removed after callers migrate; permanent aliases and silent dual formats are not supported.
