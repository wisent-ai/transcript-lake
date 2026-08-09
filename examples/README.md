# Transcript Lake examples

This is the canonical catalog of supported user outcomes for the development channel. Each example uses the installed `transcript-lake` interface, names its risks and side effects, includes observable verification and a representative failure path, and owns its cleanup decision.

No example in this catalog has been executed in a controlled clean environment during the current change. `Draft — execution pending` is evidence status, not a claim that the workflow was qualified for release. Preview or stable publication requires bounded, redacted observed evidence for every safe local example and separately controlled qualification for provider-facing or destructive work.

## Shared prerequisites

- macOS and Node.js version twenty or newer.
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

| Actor | Outcome | Interface | Preconditions | Risk | Canonical example | Evidence status |
|---|---|---|---|---|---|---|
| New operator | Install and create first observable archive | CLI | Clean supported host; local sessions optional | Local mutation, provider-facing | [First local archive](getting-started/first-local-archive.md) | Draft — execution pending |
| Operator | Read safe zero-state guidance and product identity | CLI | Installed product | Read-only | [Inspect zero state](core/inspect-zero-state.md) | Draft — execution pending |
| Operator | Tour the complete installed command surface | CLI | Installed product; optional tools per command | Mixed, explicitly staged | [CLI tour](core/cli-tour.md) | Draft — execution pending |
| Operator | Resume an incremental ingest | CLI | Existing valid Lake | Local mutation, provider-facing | [Incremental ingest](core/incremental-ingest.md) | Draft — execution pending |
| Operator | Ingest only one runtime | CLI | Supported runtime store | Local mutation, provider-facing | [Select one runtime](core/select-one-runtime.md) | Draft — execution pending |
| Analyst | Retrieve cross-runtime session evidence and locate events by literal text | CLI/DuckDB | Lake partitions; DuckDB | Read-only, external tool | [Query sessions](core/query-sessions.md) | Draft — execution pending |
| Operator | Read one past conversation back in full, in order | CLI/DuckDB | Lake partitions; DuckDB; a session id | Read-only, external tool | [Restore a conversation](core/restore-a-conversation.md) | Draft — execution pending |
| Analyst | Join Lake with Oko signal state | CLI/DuckDB | Lake, DuckDB SQLite extension, Oko index | Read-only, external tool | [Cross-source signals](integrations/duckdb/cross-source-signals.md) | Draft — execution pending |
| Operator | Create and clean Parquet mirrors | CLI/DuckDB | Lake partitions; DuckDB | Derived mutation, external tool | [Compact to Parquet](operations/compact-to-parquet.md) | Draft — execution pending |
| Oko operator | Materialize every supported runtime | CLI/files | Lake partitions | Derived mutation | [Export for Oko](integrations/oko/export-for-oko.md) | Draft — execution pending |
| Oko operator | Reindex Oko after export | CLI/process | Compatible `oko-cli` | Derived mutation, external tool | [Reindex Oko](integrations/oko/reindex-oko.md) | Draft — execution pending |
| Hook maintainer | Ingest validated Tama segments without legacy duplication | CLI/files | Tama ready directory | Local mutation, provider-facing | [Import hook decisions](integrations/tama/import-hook-decisions.md) | Draft — execution pending |
| Operator | Rebuild after source rewrite or cursor damage | CLI | Preserved old Lake; empty replacement root | Destructive/recovery | [Rebuild into an empty root](recovery/rebuild-into-empty-root.md) | Draft — execution pending |
| Operator | Upgrade or roll back exact artifacts | CLI/npm/GitHub release | Immutable archive, checksum, backup | Destructive/recovery, network install | [Upgrade and rollback](operations/upgrade-and-rollback.md) | Draft — release pending |
| Operator | Clean derived state and uninstall | CLI/npm | Stopped writers | Destructive/recovery | [Reset and uninstall](operations/reset-and-uninstall.md) | Draft — execution pending |
| Release owner | Build attributable immutable release assets | Release script | Clean exact tag; qualification approval | Local mutation, publication preparation | [Build release assets](operations/build-release-assets.md) | Draft — release pending |
| Operator | Diagnose invalid input, dependency outage, partial ingest, and writer conflict | CLI | Scenario-specific | Read-only or isolated local mutation | [Representative failures](failures/representative-failures.md) | Draft — execution pending |
| User | Ingest Gemini or Qwen | — | — | — | Not supported; see [product boundaries](../README.md#explicit-non-goals) | Not supported |
| User | Schedule ingestion | External scheduler | Operator-defined | External | Not a Transcript Lake interface; operator-managed per [README](../README.md#product-boundaries) | External |
| User | Delete individual events or mutate vendor transcripts | — | — | Destructive | Not supported | Not supported |

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
- Stop schedulers before moving, replacing, or deleting a Lake root.
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

Automated test coverage is a separate final product stage. These examples define user-comprehensible workflows and honest evidence requirements; they do not claim regression protection.
