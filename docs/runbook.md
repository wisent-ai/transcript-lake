# Operator runbook

Preserve evidence before intervention. Never edit vendor transcripts, partitions, or cursors in place. Use `transcript-lake paths`, `sources`, `doctor`, and `status --json` to identify the selected root and failure domain.

## No supported roots

**Message:** `stream found no supported source roots on this machine`

Run `sources`. Start at least one supported runtime as the same user or correct `HOME`. For Tama, verify `HOOKS_ADAPTIVE_SEGMENTS_READY`. Do not create fake directories in a production home merely to silence the refusal.

## Writer lease is held

**Message:** `state writer lock is held by <host> pid <pid>`

Run `status`; check the supervisor/service for the reported process. If it is alive, let that one stream own the root and stop the duplicate. Do not delete `stream.lock` for a live owner. The lease code automatically reclaims a provably dead owner after host/pid/start checks. If process identity cannot be established, preserve the root and escalate rather than forcing mutation.

## Cursor store is unreadable or corrupt

**Messages:**

- `cursor store is unreadable; preserve it and recover into an empty LAKE_DATA: <error>`
- `cursor store is corrupt; preserve it and recover into an empty LAKE_DATA: <error>`
- `cursor store must be a JSON object; preserve it and recover into an empty LAKE_DATA`
- `cursor record contains invalid numeric state`

Stop the writer. Preserve the complete Lake and cursor file. Do not reset `cursors.json` beside existing partitions: replay could duplicate evidence. Choose a different empty destination and run:

```sh
transcript-lake rebuild --to "$HOME/.transcript-lake-rebuild"
transcript-lake --data-dir "$HOME/.transcript-lake-rebuild" doctor
transcript-lake --data-dir "$HOME/.transcript-lake-rebuild" status
```

Switch consumers only after inspecting the new root. Retain the old root until the operator approves disposal.

## Source shrank or changed without an append

**Messages:**

- `source shrank after its last checkpoint; preserve the Lake and use rebuild`
- `source changed without an append; preserve the Lake and use rebuild`

A vendor rotated, truncated, or rewrote a tracked file. Preserve both vendor source and Lake. Rebuild to a different empty root. Optional `--source <runtime>` narrows replay only when the operator has established that other runtimes are unaffected.

Related guardrails:

- `rebuild requires --to <empty-path>`
- `rebuild target must differ from the current Lake`
- `rebuild requires an empty LAKE_DATA root so replay cannot duplicate or erase existing evidence`

## Stream watcher failures

**Messages:**

- `stream could not start: <error>`
- `stream could not watch <path>: <error>`
- `stat failed for <path>: <error>`

Check path existence, current-user read permission, file-descriptor/watch limits, and whether the root disappeared during startup. `status` reports `degraded` with paths/error after catch-up or path failures. Fix the underlying filesystem issue and restart; cursor recovery catches up complete appends.

## Hook segment conflict

**Message:** `hook segment output conflict: <path>`

An immutable Tama segment would map to an existing Lake output with different content. Stop publication, preserve both files and Tama segment metadata, and investigate producer/release identity. Never overwrite the existing partition. A correct replay of identical content is idempotent.

## DuckDB unavailable or fails

**Messages:**

- `duckdb failed to start: <error>`
- `duckdb terminated by signal`
- `duckdb exited with status <code>: <stderr>`
- `compact: duckdb failed for <runtime>`

Streaming is unaffected. Install the supported DuckDB `1.5.x` CLI on `PATH`, then retry only the read/compact command. When `TRANSCRIPT_LAKE_SQL` is set, `missing <path> (TRANSCRIPT_LAKE_SQL is set but incomplete)` means the override directory lacks the requested `views.sql` or `signals.sql`; complete it or unset the override.

## Oko integration unavailable

**Message:** `oko-cli is not on PATH; install Oko or set OKO_CLI, then run: oko-cli transcripts reindex`

Core Lake health is unaffected. Install a compatible CLI or set `OKO_CLI` to the executable. `rebuild-oko` can materialize export files without Oko; `--reindex` and `oko-refresh` require it.

Malformed export refusals:

- `full Oko export refused malformed Lake rows; authoritative partitions were not modified`
- `incremental Oko export refused malformed Lake rows; export cursor was not advanced`

Preserve the partition and export state. Identify the malformed canonical row without modifying the authoritative file. Repair at the source/code level and rebuild to qualified data; never advance `export-cursors.json` manually.

## Invalid flags and bounded values

Common messages:

- `unknown <command> flag: --<name>`
- `<command> received duplicate --<name>`
- `--<name> requires a value`
- `--limit must be an integer from 1 to 500`
- `--days must be an integer from 1 to 500`
- `unknown source "<value>" (expected one of: claude, codex, omp, droid, kimi, hooks)`

Use `transcript-lake help <command>`. Do not make automation parse human help; prefer supported `--json` output.

## Label refusals

- `--aspect must be a non-empty string`
- `--value must be a non-empty string`
- `--source must match ^(manual|human|model|brama)(:[A-Za-z0-9._/-]+)?$ (manual, human, model, or brama, with an optional :detail suffix)`
- `unknown session "<id>": not present in the selected Lake (check the id or start the stream first)`

Confirm the selected root and session id. If the same id exists in multiple runtimes, provide `--runtime`. Never place a secret in `--value` or `--note`; labels are not masked.

## Local goal model failures

- `local goal model requires llama-cli on PATH or TRANSCRIPT_LAKE_GOAL_LLAMA_CLI`
- `<ENV_KEY> does not name a file: <path>`
- `local goal model returned no goal tag`
- `local goal model returned no opening goal tag`
- `local goal model returned an invalid <n>-word title`

Check [configuration](configuration.md), pinned artifact integrity, and local disk/network availability for first acquisition. This feature is optional and does not affect streaming. Do not replace the pinned model/prompt with an unqualified artifact.

## Derived cleanup

Always preview:

```sh
transcript-lake clean --target all
```

Expected footer: `preview only; add --apply to remove derived data`. Apply only after confirming paths. Invalid targets return `--target must be parquet, oko, or all`. `clean` never removes events, cursors, or labels.

## Escalation bundle

Provide public-safe, synthetic or redacted evidence only:

- exact binary version and immutable release coordinate;
- `paths --json`, `doctor --json`, `status --json` with local usernames/paths/session identifiers redacted;
- exact error string and exit status;
- supervisor logs around the failure;
- whether the old root and vendor source were preserved.

Never attach real transcripts or a production Lake to an issue.