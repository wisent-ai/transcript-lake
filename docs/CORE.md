# Core functionality contract

This document defines the smallest provider-neutral system that fulfills Transcript Lake's README promise. Integrations may consume or extend these contracts but cannot redefine them.

## Workflow: stream local evidence

- **Actor:** one supervised local stream process.
- **Initial state:** a selected `LAKE_DATA`; zero or more supported local source stores; no concurrent Lake writer.
- **Input:** filesystem notifications naming transcript or closed-hook-segment files.
- **Authoritative inputs:** vendor transcript stores and closed hook segments.
- **Authoritative Lake state:** masked NDJSON partitions plus the cursor store that proves consumed source boundaries.
- **Success:** every complete new source line is masked and appended to its partition and affected Oko session before its durable byte cursor advances.
- **Source failure:** the named file's cursor does not advance; the failure is logged and other source files continue streaming.
- **Restart:** the stream resumes from durable cursors and does not repeat checkpointed source bytes.
- **Recovery:** corrupt cursor state, source truncation, and non-append rewrite are not replayed into an existing Lake; reconstruction targets a separate empty root.
- **Security:** raw text is masked before partition or projection writes. No credential is required. Source stores are read-only.

The stream is event-driven: it receives the path that changed, waits only for a short coalescing window, and reads that file from its cursor. It does not enumerate every transcript, launch a refresh command, or rely on a timer. `rebuild` is the explicit historical path and writes a separate empty Lake.

## Workflow: discover and inspect state

`transcript-lake paths`, `sources`, `doctor`, and `status` are read-only. Together they report resolved state and integration paths, discovered provider roots and file counts, cursor integrity, optional dependency presence, runtime partition counts and bytes, live stream state, and Oko freshness. `--json` exposes stable structured command results for automation.

These commands never create configuration, start streaming, repair state, contact providers, or claim that a missing optional integration invalidates core partitions. Corrupt authoritative metadata makes `doctor` and `status` exit non-zero. Missing optional DuckDB or Oko is a warning until the corresponding capability is invoked.

## Workflow: query evidence

`transcript-lake query "<sql>"` loads the frozen DuckDB views compiled into the binary over the selected Lake and executes the supplied SQL, so an installed CLI is never separable from its views. Named read commands (`sessions`, `events`, `search`, `show`, `stats`, `hooks`, `signals`, `label list`, `label aspects`) cover bounded common reads over the same views without operator SQL; `search` treats its term as literal text, escaping LIKE wildcards before matching, and `show` reconstructs one whole conversation in chronological order without per-event truncation. The query process does not mutate NDJSON, cursors, or source stores. DuckDB is an explicit optional prerequisite; absence is an actionable dependency failure, not a fallback to another engine.

The canonical `events` view pins column names and types and tolerates only a torn final partition line during a concurrent read. Aggregate views expose sessions, tools, tokens, and hook decisions. User SQL can itself create external files or perform DuckDB mutations; the operator owns the supplied SQL. Transcript Lake does not label arbitrary SQL as read-only.

## Workflow: compact derived state

`transcript-lake compact` creates per-runtime Parquet mirrors under `LAKE_DATA/parquet`. NDJSON remains authoritative and is never deleted. Existing output conflicts or DuckDB failure stop that runtime and produce a non-zero exit. Retrying is safe only after the operator inspects or removes the conflicting derived artifact.

## Public command semantics

| Command | Mutation | Success | Failure |
|---|---|---|---|
| no command / `help [command]` / `--help` | None | General or command-specific guidance | Unknown topic is non-zero |
| `--version` | None | Canonical product version, compiled in from `Cargo.toml` | The version travels inside the binary, so an artifact cannot report a version it was not built from |
| `paths` / `sources` | None | Resolved paths and discovered supported stores | Permission or adapter error is explicit |
| `doctor` | None | Health report; missing optional tools are warnings | Corrupt authoritative state or broken adapter is non-zero |
| `status` | None | Human or JSON inventory and freshness | Corrupt cursor/summary state is non-zero without repair |
| `stream` | `LAKE_DATA` only | Long-running direct source-to-Lake/Oko commits; clean stop on SIGINT/SIGTERM | Startup/configuration failure is non-zero; source failures are logged without advancing that source cursor |
| `rebuild` | Separate empty target only | Historical replay and projection in the new root | Current root preserved; invalid/non-empty target is non-zero |
| `sessions` / `events` | None | Filtered normalized evidence | Non-zero dependency or input error |
| `search` | None | Newest-first literal substring matches over event text | Non-zero dependency or input error |
| `show` | None | One conversation, oldest turn first, untruncated masked text, with a rendered/matched footer | Unknown session, unknown event type, or dependency error is non-zero |
| `label add` | `LAKE_DATA/labels` only | Appended label record with namespaced provenance (`manual`/`human`/`brama:<model>`/`model:<artifact>`); unknown or ambiguous session is rejected | Non-zero dependency or input error |
| `label list` / `label aspects` | None | Latest-assignment labels or aspect summaries | Non-zero dependency or input error |
| `stats` / `hooks` | None | Bounded aggregates or hook decisions | Non-zero dependency or input error |
| `query` | User SQL may have DuckDB-defined effects | DuckDB result | Non-zero dependency or SQL error |
| `compact` | Derived Parquet only | Filterable per-runtime size/path report | Non-zero; NDJSON preserved |
| `rebuild-oko` | Derived Oko projection only | Full reconstruction summary | Non-zero; partitions and cursors preserved |
| `oko-refresh` | Oko-owned index through `oko-cli` | Child-process success | Non-zero or actionable missing-CLI guidance |
| `clean` | Derived Parquet/Oko only with `--apply` | Dry-run by default; explicit removal report | Active writer or filesystem failure is non-zero |

Global `--data-dir <path>` selects the root for one invocation and may appear before or after the command. Unknown commands, flags, duplicate flags, contradictory input, and out-of-range limits are rejected before mutation. `--source` accepts only the declared runtime set.

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
| `events/` NDJSON | Transcript Lake | State writer lease | Append-only | Backup or rebuild into a separate root |
| `cursors.json` | Transcript Lake | Locked read-modify-write transaction | Atomic merge under state lease | Hard-fail on corruption; never silently reset |
| `stream-status.json` | Transcript Lake stream | Current stream process | Atomic replacement | Diagnostic; regenerated on start, stop, and commit |
| `parquet/` | DuckDB compaction | State writer lease | No concurrent Lake mutation | `clean --target parquet --apply`; rebuild from NDJSON |
| `exports/oko/` | Lake stream or recovery exporter | Same transaction as the corresponding canonical events | Atomic session/cursor writes | `clean --target oko --apply`; rebuild from NDJSON |
| `labels/labels.ndjson` | Operator via Transcript Lake | Append-only single-line writes | No writer lease; independent of stream | Deleting loses only labels; events and exports unaffected |
| Oko SQLite index | Oko | Oko | External single-writer contract | Rebuild from Lake export |

A writer lease fails fast when another live owner holds the root. Stale same-host claims are reclaimed only after process identity no longer matches. Shared cross-host mutation is unsupported.

## Failure classification

- **Invalid input:** unknown command, flag, or runtime; reject before mutation.
- **Missing optional dependency:** DuckDB or Oko unavailable; affected operation fails while unrelated core commands remain usable.
- **Permission failure:** name the source or state path; do not silently skip it as success.
- **Source conflict:** truncation or same-size rewrite; reject replay into existing partitions.
- **State conflict:** active writer; fail fast and preserve incumbent ownership.
- **Corrupt authoritative metadata:** cursor parse or numeric validation failure; preserve state and require separate-root recovery.
- **Source failure:** preserve completed cursors, log the named file, and continue other paths.
- **Interruption:** append, projection, and cursor ordering permits restart from the last committed source boundary.
- **Storage exhaustion:** the source cursor does not advance past the failed write. Freeing space allows a later source notification or process restart to retry it.

There is no external retry scheduler. The supervised process remains live after a source-local failure and retries that path on its next filesystem notification; startup and authoritative-state failures exit for the supervisor to restart.

## Configuration

- `--data-dir <path>`: global per-invocation state-root selection, resolved to an absolute path.
- `LAKE_DATA`: automation default when `--data-dir` is absent. Default `~/.transcript-lake`.
- `OKO_CLI`: optional integration executable override. Default path discovery searches `PATH` only when the integration is invoked.
- `HOOKS_ADAPTIVE_SEGMENTS_READY`: optional Tama handoff location used only by hook discovery and streaming.
- `TRANSCRIPT_LAKE_SQL`: optional directory of view definitions replacing the ones compiled into the binary. Used to iterate on views against an installed CLI. A directory that is set but does not contain the requested script is an error, never a silent fall back to the compiled copy.

Unknown CLI flags are rejected. Environment variables not documented here are not part of the public product contract. `paths` exposes resolved locations without transcript contents or credentials.

## Security and privacy boundary

Masking protects credential-shaped and high-entropy text before durable Lake writes, but it is not anonymization. Session identifiers, machine name, timestamps, project path, model, tool name, sizes, counts, and short non-matching text can remain sensitive. Protect `LAKE_DATA` with local filesystem permissions and do not publish it.

The masker is deterministic and idempotent. Nested metadata is depth bounded and text fields are size capped. Full tool-result files are referenced but never followed. No core command transmits transcript data over the network.

## Resource behavior

The stream reads changed files from newline-aligned byte cursors and bounds each in-memory commit. Individual text fields are capped and nested extra metadata is depth bounded. Session count and total Lake disk usage are not globally capped because they reflect the history appended by supported runtimes; operators control storage with filesystem quotas and use explicit separate-root rebuilds for historical scope.

The product supports one writer and concurrent read-only status or query operations per local state root. It defines no shared-filesystem, multi-host, or distributed-write topology.

## Evolution

Removal or incompatible change to commands, flags, runtime identifiers, configuration, event fields, cursor semantics, partition layout, masking guarantees, or integration export schemas follows [release policy](RELEASES.md). Stored-state changes require explicit migration and rollback analysis. Obsolete paths are removed after callers migrate; permanent aliases and silent dual formats are not supported.
