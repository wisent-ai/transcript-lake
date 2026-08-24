# CLI reference

```text
transcript-lake [--data-dir <path>] <command> [flags]
```

No command, `-h`, or `--help` prints general help without reading or writing state. `-V`/`--version` prints the package version. `help [command]` accepts at most one exact command name. Global `--data-dir` may appear anywhere, must appear once, and selects an absolute root for the invocation and child processes.

Flags are long-form, exact, and single-use. Unknown or duplicate flags exit 1. Unless noted, a successful command exits 0 and failures exit 1. Commands supporting `--json` write machine-readable JSON; the long-running stream writes one JSON record per status line.

Defaults: ordinary `--limit` 20 (range 1–500), `show --limit` 2,000 (range 1–50,000), `stats --days` 7 (range 1–500). Runtime/source values are `claude|codex|omp|droid|kimi|hooks`.

## Discover and inspect

| Command | Flags | Behavior / output |
|---|---|---|
| `paths` | `--json` | Resolved `dataDir`, events, cursors, `streamStatus`, parquet, `okoExport`, `okoStaging`, `hookSegments`, DuckDB, and `okoCli`. Read-only. |
| `sources` | `--json` | One row per runtime: `runtime`, `available`, `mode`, `roots`, `files`, optional `error`. Non-zero only when a source enumeration error exists; absence is normal. |
| `doctor` | `--json` | Checks `state-root`, `cursors`, `sources`, `source-integrity`, `duckdb`, and `oko-cli`. Missing optional dependencies/sources are warnings. JSON: `dataDir`, `healthy`, `checks`. Non-zero for an `error` check. |
| `status` | `--json` | `dataDir`, partition counts/bytes, cursor state/freshness, stream state, Oko/Lake freshness. Non-zero for invalid cursor or stream-status JSON. |

## Stream and recover

| Command | Flags | Behavior / output |
|---|---|---|
| `stream` | `--json` | Watches supported roots, installs watches before catch-up, then commits appends until `SIGINT`/`SIGTERM`. Human logs: timestamp, `stream`, kind, key/value details. JSON logs: `ts`, `kind`, details. Requires one writer lease. |
| `rebuild` | required `--to <empty-path>`; optional `--source <runtime>` | Replays selected vendor history through the same masker into a different empty root. Refuses current/non-empty Lake. Summary: `perRuntime`, `maskCounts`, `durationMs`, `partial`, `failures`. |

## Read and analyze

These commands use DuckDB `1.5.x`, except inspection above.

| Command | Flags/arguments | Behavior |
|---|---|---|
| `sessions` | `--runtime <r>`, `--project <text>`, `--interrupted`, `--limit <n>`, `--json` | Newest sessions. Project filter is case-insensitive substring. `--interrupted` selects diagnosis fields (`stopped_as`, final user opening) instead of token totals. |
| `events` | `--runtime <r>`, `--session <id>`, `--type <type>`, `--limit <n>`, `--json` | Newest events; displayed text is capped to 240 characters. |
| `search <text>` | same filters as `events`; `--json` | Case-insensitive literal substring over masked event text. `%`, `_`, and escape characters are neutralized, not treated as patterns. Multiple positionals join with spaces. |
| `show <session-id>` | `--include <csv|all>`, `--limit <n>`, `--json` | Chronological full text for selected types. Default `user,assistant`. JSON includes identity, `include`, `matched`, `rendered`, `events`; human output ends `rendered N of M matching events`. |
| `stats` | `--days <n>`, `--runtime <r>`, `--json` | Per-runtime event/session/message/tool counts, reported tokens, first/last timestamps within the window. |
| `hooks` | `--decision <value>`, `--tool <hook-id>`, `--limit <n>`, `--json` | Newest hook decisions with project, decision, hook event, infra, and masked reason. |
| `signals` | `--report <freshness|frustration|overlap|daily>`, `--limit <n>`, `--json` | Loads Oko-joined SQL views. Default `freshness`. Requires DuckDB SQLite extension and Oko index. |
| `query "<sql>"` | `--json` | Executes operator SQL after loading canonical Lake views. The SQL text is positional and joined with spaces. |

Event `--type` is not prevalidated in `events`/`search`; an unknown spelling safely returns no matching rows. `show --include` is validated against all seven canonical types.

## Labels and local goal titles

| Command | Flags/arguments | Behavior |
|---|---|---|
| `label add <session-id>` | required `--aspect <a> --value <v>`; optional `--note <text> --runtime <r> --source <provenance> --json` | Resolves exactly one existing session (or the requested runtime), then appends and fsyncs one label. Source defaults `manual`. |
| `label list` | `--session <id>`, `--aspect <a>`, `--runtime <r>`, `--limit <n>`, `--json` | Latest assignment per session/aspect, newest first. |
| `label aspects` | `--json` | Aspect/value counts and distinct labeled sessions. |
| `goal title` | exactly one of `--text <text>` or `--stdin`; `--json` | Runs the SHA-pinned qualified GGUF locally. Human output is the title or `(no goal)`. May fetch pinned artifacts if absent. |
| `goal label <session-id>` | `--runtime <r>`, `--json` | Reads the first masked user prompt, infers a 3–12 word title, and appends a label with source `model:jeden-goal-qwen3-4b-2512d7a4`. |

## Derived data and Oko

| Command | Flags | Behavior |
|---|---|---|
| `compact` | `--source <runtime>`, `--json` | Rebuilds Parquet per selected/all runtime. JSON rows: `runtime`, `sourceBytes`, `parquetBytes`, `output`, `status`. NDJSON remains authoritative. |
| `rebuild-oko` | `--reindex` | Full Oko projection rebuild. Summary: `outputRoot`, `sessions`, `records`, `written`, `unchanged`, `pruned`, `mode`, `malformed`, `durationMs`, optional `lastError`/`reindex`. `--reindex` invokes `oko-cli transcripts reindex --json`. |
| `oko-refresh` | none | Invokes configured/found `oko-cli transcripts reindex`; returns its exit status. |
| `clean` | `--target <parquet|oko|all>`, `--apply`, `--json` | Default target `all`; default preview. Reports target/path/existence/bytes/applied. Removes only rebuildable Parquet, Oko export, and Oko staging trees. |

## Stable JSON schemas

Canonical events and Oko export lines are documented in [ingestion reference](ingestion-reference.md) and [export concept](concepts/export.md). Operational JSON is intended for automation but may gain additive fields; consumers should ignore unknown keys. Human output is for operators.

See [configuration](configuration.md) and [runbook](runbook.md).