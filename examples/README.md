# Transcript Lake examples

This is the canonical catalog of supported user outcomes for the development channel. Each example uses the installed `transcript-lake` interface, names its risks and side effects, includes observable verification and a representative failure path, and owns its cleanup decision.


## Shared prerequisites

- macOS. Building the CLI needs a Rust toolchain at version `1.85` or newer; running the installed binary needs nothing else.
- Installed Transcript Lake development build or exact release archive.
- An operator-owned `LAKE_DATA` root.
- DuckDB `1.5.x` only for query, signal, and compact examples.
- Oko only for reindex examples.
- No production credentials are required or accepted.

Every example assumes the user has read [onboarding](../docs/ONBOARDING.md). Replace placeholders only with operator-owned local paths or exact immutable versions. Never use a shared production Lake for a destructive or recovery example.

## Risk labels

- **Read-only:** does not intentionally mutate product or provider state.
- **Local mutation:** writes only operator-selected local product state.
- **Derived mutation:** writes rebuildable Parquet or export artifacts.
- **Destructive/recovery:** changes which state root is active or removes retained local data; requires explicit operator decision.
- **Provider-facing:** reads local files owned by coding-agent runtimes; never modifies them.
- **External tool:** invokes DuckDB or Oko and inherits that tool's local behavior.

No catalog example is billable, network-facing at runtime, or credentialed. Release download and publication are separate network operations.

## Coverage matrix

| Actor | Outcome | Interface | Preconditions | Risk | Canonical example |
|---|---|---|---|---|---|
| New operator | Install and create the first live archive | CLI | Clean supported host; local sessions optional | Local mutation, provider-facing | [First local archive](getting-started/first-local-archive.md) |
| Operator | Read safe zero-state guidance and product identity | CLI | Installed product | Read-only | [Inspect zero state](core/inspect-zero-state.md) |
| Operator | Tour the complete installed command surface | CLI | Installed product; optional tools per command | Mixed, explicitly staged | [CLI tour](core/cli-tour.md) |
| Operator | Keep an existing Lake current | CLI | Existing valid Lake | Local mutation, provider-facing | [Live stream](core/live-stream.md) |
| Analyst | Retrieve cross-runtime session evidence and locate events by literal text | CLI/DuckDB | Lake partitions; DuckDB | Read-only, external tool | [Query sessions](core/query-sessions.md) |
| Operator | Read one past conversation back in full, in order | CLI/DuckDB | Lake partitions; DuckDB; a session id | Read-only, external tool | [Restore a conversation](core/restore-a-conversation.md) |
| Analyst | Join Lake with Oko signal state | CLI/DuckDB | Lake, DuckDB SQLite extension, Oko index | Read-only, external tool | [Cross-source signals](integrations/duckdb/cross-source-signals.md) |
| Operator | Create and clean Parquet mirrors | CLI/DuckDB | Lake partitions; DuckDB | Derived mutation, external tool | [Compact to Parquet](operations/compact-to-parquet.md) |
| Oko operator | Reconstruct every supported runtime projection | CLI/files | Lake partitions | Derived mutation | [Rebuild Oko projection](integrations/oko/rebuild-oko.md) |
| Oko operator | Reindex Oko after projection | CLI/process | Compatible `oko-cli` | Derived mutation, external tool | [Reindex Oko](integrations/oko/reindex-oko.md) |
| Hook maintainer | Stream validated Tama segments without legacy duplication | CLI/files | Tama ready directory | Local mutation, provider-facing | [Import hook decisions](integrations/tama/import-hook-decisions.md) |
| Operator | Rebuild after source rewrite or cursor damage | CLI | Preserved old Lake; empty replacement root | Destructive/recovery | [Rebuild into an empty root](recovery/rebuild-into-empty-root.md) |
| Operator | Upgrade or roll back exact artifacts | CLI/GitHub release | Immutable archive, checksum, backup | Destructive/recovery, network install | [Upgrade and rollback](operations/upgrade-and-rollback.md) |
| Operator | Clean derived state and uninstall | CLI | Stopped stream and writers | Destructive/recovery | [Reset and uninstall](operations/reset-and-uninstall.md) |
| Release owner | Build attributable immutable release assets | Release script | Clean exact tag; qualification approval | Local mutation, publication preparation | [Build release assets](operations/build-release-assets.md) |
| Operator | Diagnose invalid input, dependency outage, source failure, and writer conflict | CLI | Scenario-specific | Read-only or isolated local mutation | [Representative failures](failures/representative-failures.md) |
| User | Stream Gemini or Qwen | — | — | — | Not supported; see [product boundaries](../README.md#explicit-non-goals) |
| User | Delete individual events or mutate vendor transcripts | — | — | Destructive | Not supported |

## Selecting and running an example

1. Choose one row matching the desired outcome.
2. Read its status, risk, environment, preconditions, inputs, side effects, and failure path before copying commands.
3. Pass an isolated root with global `--data-dir` for local mutation, recovery, or failure work.
4. Run the stated installed `transcript-lake` commands; source-tree scripts appear only in the release-maintainer example.
5. Compare the observable result with the expected shape; do not interpret exit zero alone as success.
6. Follow the explicit cleanup or retention decision.

## Global safety and cleanup

- Never point recovery examples at the only copy of a Lake.
- Never delete vendor transcript stores; examples do not require it.
- Stop the stream before moving, replacing, or deleting a Lake root.
- Treat Lake metadata and unmasked short text as sensitive even after secret masking.
- Examples create no cloud resources and request no credentials.
- Delete only paths created or explicitly selected by the example.
- Preserve failure evidence before cleanup when diagnosing corruption or partial state.

## Related contracts

- [Product README](../README.md)
- [Onboarding](../docs/ONBOARDING.md)
- [Core functionality](../docs/CORE.md)
- [Integration contracts](../docs/INTEGRATIONS.md)
- [Release policy](../docs/RELEASES.md)
- [Architecture and data model](../docs/LAKE.md)

