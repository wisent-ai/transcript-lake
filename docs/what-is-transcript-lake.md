# What is Transcript Lake?

Transcript Lake is the local, privacy-masked canonical archive for coding-agent conversations. It reads supported vendor transcript stores without modifying them, normalizes their records into one event schema, masks credential-shaped text before durable output, and keeps append-only daily NDJSON partitions that can be queried through DuckDB.

It is useful when one machine has conversations in Claude Code, Codex, omp, Factory Droid, Kimi, and Tama hook telemetry and the operator needs one inspectable history instead of six vendor-specific formats.

## The four-owner boundary

| Owner | Owns | Relationship to the Lake |
|---|---|---|
| Vendor runtimes | Raw transcript stores on disk | Transcript Lake reads them; it never rewrites, migrates, adopts, or deletes them. |
| Transcript Lake | Masked canonical partitions, durable cursors, and `exports/oko` | This repository defines and writes these artifacts. |
| Oko | Its merged catalogue, rebuildable search projection, sessions its broker started, and Oko bookkeeping | Oko scans the Lake export read-only. It does not mask transcripts or write under `LAKE_DATA`. |
| Oko Desktop | Native application lifecycle and UI | It consumes Oko; it replaces neither Transcript Lake nor the Oko executables. |

The boundary is operational: deleting a vendor store destroys vendor-owned history; deleting Lake partitions destroys the canonical masked archive; deleting an Oko catalogue removes a rebuildable index, not the archive.

## What it does

1. Discovers supported transcript roots under the current `HOME`.
2. Watches existing roots and catches up from newline-aligned byte cursors.
3. Parses vendor records into provider-neutral raw events.
4. Applies the single masking boundary to `text` and retained string data.
5. Caps retained values and writes canonical events to daily runtime partitions.
6. Materializes the affected per-session Oko export.
7. Advances the source cursor only after durable outputs succeed.
8. Exposes bounded read commands, DuckDB views, labels, Parquet mirrors, and recovery replay.

## What it does not do

- It does not own or mutate raw transcripts.
- It does not promise anonymization. Project paths, timestamps, session identifiers, model names, tool names, machine identity, short secrets outside the documented patterns, and ordinary conversation text can remain sensitive.
- It does not route model calls, own provider credentials, or manage remote retained history.
- It does not make Oko the archive. `exports/oko` is a rebuildable projection of the Lake.
- It does not automatically delete authoritative events or individual records.
- It does not support Gemini or Qwen transcript adapters in this revision.

## Start here

- [Executed quick start](quick-start.md)
- [Core concepts](concepts/event.md)
- [CLI reference](cli-reference.md)
- [Masking guarantees](masking-guarantees.md)
- [Runbook](runbook.md)
- [Examples catalogue](../examples/README.md)

The existing [data contract](LAKE.md), [core contract](CORE.md), [integration contracts](INTEGRATIONS.md), [onboarding guide](ONBOARDING.md), and [release policy](RELEASES.md) remain normative.