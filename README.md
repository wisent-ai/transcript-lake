# Transcript Lake

<!-- wisent-readme-signals:start -->
[![Release](https://img.shields.io/github/v/release/wisent-ai/transcript-lake?display_name=tag&sort=semver)](https://github.com/wisent-ai/transcript-lake/releases)
[![Downloads](https://img.shields.io/github/downloads/wisent-ai/transcript-lake/total)](https://github.com/wisent-ai/transcript-lake/releases)
[![License](https://img.shields.io/github/license/wisent-ai/transcript-lake)](https://github.com/wisent-ai/transcript-lake)
[![Discord](https://img.shields.io/badge/Discord-Join%20Wisent-5865F2?logo=discord&logoColor=white)](https://discord.gg/qRjpkthq54)
<!-- wisent-readme-signals:end -->


Transcript Lake turns local coding-agent conversations into one privacy-masked, incrementally updated event archive that operators can inspect with SQL and import into Oko.

## Problem and intended users

Coding agents persist conversations in incompatible local formats. Comparing activity across Claude Code, Codex, OMP, Droid, Kimi, and adaptive hooks otherwise requires provider-specific parsers, repeated full scans, and unsafe handling of raw transcripts.

Transcript Lake is for:

- **individual developers** who need one local history without copying raw credentials into an analytics store;
- **engineering and product operators** who need cross-runtime session, tool, token, and hook-decision evidence;
- **Oko operators** who need a stable provider-neutral transcript feed rather than another set of vendor parsers;
- **adapter maintainers** who add support for a verified local transcript format behind one canonical event contract.

Its value is a single ingestion and masking boundary: every downstream consumer reads the same normalized evidence instead of independently handling raw vendor files.

## Product boundaries

### Included

- Incremental, newline-aligned ingestion from local Claude Code, Codex, OMP, Droid, and Kimi session stores.
- Import of adaptive-hook decisions from the local Tama telemetry log when present.
- Provider-neutral NDJSON events for user, assistant, thinking, tool, usage, metadata, and hook-decision records.
- Deterministic masking before any event reaches durable Lake storage.
- Cursor-based restart, append-only daily partitions, status reporting, DuckDB views, and Parquet compaction.
- A canonical, per-session export consumed by Oko.

### Explicit non-goals

- Transcript Lake is not a chat UI, agent runtime, cloud service, or team synchronization system.
- It does not modify vendor transcript stores.
- It does not provide semantic or vector search.
- It does not currently ship Gemini or Qwen adapters.
- It does not fully anonymize conversations. Hostname, runtime, timestamps, session identifiers, project paths, model names, and bounded structural metadata remain available for correlation.
- It does not schedule ingestion. An operator or external scheduler invokes the CLI.

### Environment and constraints

- Current source formats and paths are supported on macOS. Other operating systems are not qualified.
- Node.js is required for every workflow. The implementation uses only Node built-ins.
- DuckDB CLI `1.5.x` is required only for SQL queries and Parquet compaction.
- Oko and Tama are optional integrations. Core ingestion remains usable without them.
- Data remains local under `LAKE_DATA`, defaulting to `~/.transcript-lake`.
- A full ingest can read the entire available local transcript history and consume proportional disk space. It is never automatic.

## Core use cases

| Actor | Initial situation | Desired result | Product action | Safety and cost boundary |
|---|---|---|---|---|
| Developer | Several supported agents have local session files | One masked, resumable local archive | Run `transcript-lake ingest` | Reads local transcripts; writes only beneath `LAKE_DATA` |
| Analyst | Lake partitions exist | Cross-runtime session, tool, token, or hook evidence | Run `transcript-lake query "<sql>"` | Read-only over Lake data; requires local DuckDB |
| Operator | NDJSON partitions consume too much scan time | Immutable additive Parquet mirrors | Run `transcript-lake compact` | Adds files; never deletes NDJSON partitions |
| Oko operator | Oko must index multiple runtimes consistently | Stable per-session canonical JSONL | Run ingest or `transcript-lake export-oko` | Export contains masked Lake events and local metadata |
| Maintainer | Oko index may be stale | Refresh Oko after export | Run `transcript-lake export-oko --reindex` | Optional child process; failure does not mutate Lake partitions |

## How the product works

```mermaid
flowchart LR
    A[Local vendor transcript stores] --> B[Runtime adapters]
    H[Adaptive hook telemetry] --> B
    B --> C[Canonical in-memory events]
    C --> D[Masking boundary]
    D --> E[NDJSON partitions and cursors]
    E --> F[DuckDB views and Parquet mirrors]
    E --> G[Provider-neutral Oko export]
```

Raw vendor text exists only on the source side of the masking boundary. Adapters translate source records but do not write them. The ingest driver masks `text` and every string in `extra` before appending canonical events to `LAKE_DATA/events`.

`LAKE_DATA/cursors.json` is the ingestion checkpoint and the source of incremental progress. Daily NDJSON partitions are authoritative Lake evidence. Parquet files and the Oko export are rebuildable derived views. Cursor and export metadata use atomic replacement; transcript partitions are append-only.

See [the core workflow contract](docs/CORE.md) and [the data and architecture contract](docs/LAKE.md) for state transitions, schemas, masking behavior, paths, and recovery semantics.

## Quick start

There is not yet a supported immutable release. The current **development channel** is installed from a source checkout and carries no stability guarantee.

### Prerequisites

- macOS with at least one supported coding-agent transcript store;
- Node.js available as `node`;
- DuckDB `1.5.x` on `PATH` only if you will query or compact data;
- enough local storage for the masked copy of the selected history.

```sh
git clone https://github.com/wisent-ai/transcript-lake.git
cd transcript-lake
npm install --global .
transcript-lake
```

With no command, the CLI prints its purpose and safe starting commands without creating Lake state. Inspect the environment before mutation:

```sh
transcript-lake paths
transcript-lake sources
transcript-lake doctor
transcript-lake --data-dir "$HOME/.transcript-lake" status
```

To produce the first local result:

```sh
transcript-lake --data-dir "$HOME/.transcript-lake" ingest
transcript-lake --data-dir "$HOME/.transcript-lake" sessions --limit 10
transcript-lake --data-dir "$HOME/.transcript-lake" stats --days 7
```

Expected result: `ingest` prints a JSON summary containing per-runtime counts, masking counts, duration, and Oko-export status. `sessions` lists recent normalized conversations and `stats` aggregates local evidence through DuckDB. If no supported source store exists, ingest produces an empty but valid Lake rather than fabricated data.

This workflow reads local transcripts and creates or updates only the selected data root. Vendor transcripts are never changed. `clean` previews removal of rebuildable derived data; authoritative partitions require an explicit operator retention decision outside the CLI.

Continue with the [CLI tour](examples/core/cli-tour.md), [full onboarding guide](docs/ONBOARDING.md), and [canonical examples catalog](examples/README.md).

## Primary interfaces

The CLI is the canonical human and automation interface.

| Operation | Interface | Observable result |
|---|---|---|
| Guidance and identity | `transcript-lake help [command]`, `--version` | Exact syntax, safety guidance, or canonical version |
| Paths and discovery | `paths`, `sources` | Resolved state/integration paths and available runtime stores |
| Health | `doctor [--json]` | Cursor, source, DuckDB, and Oko checks with meaningful exit status |
| Ingest | `ingest [--source <runtime>] [--full]` | Structured run summary and durable cursors/partitions |
| Online freshness | `watch [--debounce <seconds>] [--json]` | Long-running watcher firing the standard refresh on source changes |
| Safe recovery | `rebuild --to <empty-path> [--source <runtime>]` | Full replay into a separate empty Lake |
| Inspect | `status [--json]` | Partition, cursor, last-ingest, and Oko freshness inventory |
| Sessions and events | `sessions [--interrupted]`, `events` | Filtered recent normalized records; `--interrupted` keeps only conversations left without an answer |
| Text search | `search <text> [--runtime <r>] [--session <id>] [--type <t>] [--limit <n>] [--json]` | Newest-first literal substring matches over masked event text |
| Session labels | `label add`, `label list`, `label aspects` | Operator-owned aspect/value annotations over sessions |
| Statistics and signals | `stats`, `hooks`, `signals` | Usage aggregates, adaptive-hook decisions, and Oko/Lake correlations |
| Advanced SQL | `query [--json] \"<sql>\"` | DuckDB result or actionable dependency error |
| Compact | `compact [--source <runtime>] [--json]` | Per-runtime NDJSON-to-Parquet report |
| Export and refresh Oko | `export-oko [--full] [--reindex]`, `oko-refresh` | Export summary and optional Oko reindex |
| Derived cleanup | `clean [--target <parquet|oko|all>] [--apply]` | Dry-run by default; removes rebuildable data only with `--apply` |

Canonical event and adapter interfaces are machine contracts documented in [the architecture contract](docs/LAKE.md). Every supported operation maps to [one canonical example](examples/README.md).

## Operational model

- **Configuration:** global `--data-dir <path>` selects the state root for one invocation; `LAKE_DATA` remains the automation default. `OKO_CLI` optionally selects the Oko executable. Unset values use documented local defaults; there are no credential fallbacks.
- **State ownership:** vendor runtimes own source transcripts; Transcript Lake alone owns `LAKE_DATA`; Oko owns its SQLite index and imports a derived Lake export read-only.
- **Credentials:** core ingestion needs none. Transcript contents may contain credentials, so masking occurs before durable Lake writes. Do not share a Lake directory as though it were anonymized data.
- **Upgrades:** use an immutable release once available. State layout compatibility, rollback, and release channels are defined in [release policy](docs/RELEASES.md).
- **Observability:** `paths`, `sources`, `doctor`, `status --json`, and structured mutation summaries expose configuration, availability, freshness, counts, and failures.
- **Recovery:** rerun incremental ingest after interruption. A source truncation, same-size rewrite, or damaged cursor is rejected before replay; preserve the current Lake and use `rebuild --to <empty-path>` for a separate full reconstruction.
- **Retention:** no automatic authoritative deletion is performed. `clean` handles only rebuildable Parquet and Oko artifacts, previews by default, and requires `--apply`.
- **Integrations:** capability, dependency, failure, and removal contracts are in [integration contracts](docs/INTEGRATIONS.md).

## Project status and support

- **Maturity:** development (`0.x` contract; no supported immutable release yet).
- **Current compatibility:** macOS; Node.js; DuckDB `1.5.x` for SQL/compaction; locally observed formats for Claude Code, Codex, OMP, Droid, Kimi, and Tama hook telemetry.
- **Support and defects:** [GitHub Issues](https://github.com/wisent-ai/transcript-lake/issues).
- **Security reports:** use a private [GitHub security advisory](https://github.com/wisent-ai/transcript-lake/security/advisories/new); do not disclose transcript data or credentials in a public issue.
- **License:** MIT; see [LICENSE](LICENSE).

Current behavior is described in this README. Planned capabilities must be labeled as planned and are not supported until released with matching examples and evidence.
