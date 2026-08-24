# Configuration reference

Transcript Lake has no configuration file. Configuration is the global `--data-dir` flag, environment variables, command flags, and external binaries on `PATH`. Empty environment values are treated as unset unless stated otherwise.

## Global selection

| Key | Type/default | Precedence and effect |
|---|---|---|
| `--data-dir <path>` | path; absent | May appear anywhere before/after the command. Converted to absolute, exported to the child process as `LAKE_DATA`, and wins for that invocation. Duplicate flag or missing path is an error. |
| `LAKE_DATA` | path; `$HOME/.transcript-lake` | State root for events, cursors, labels, status, derived data, and model cache. Relative paths are made absolute from the invocation directory. |
| `HOME` | path; `.` if unavailable | Vendor roots, default Lake root, default Tama ready path, shared goal artifacts, and Oko index location. Run under the same real user as the transcript stores. |
| `PATH` | path list | Discovery of `duckdb`, `oko-cli`, and `llama-cli`; child-process execution. |

## Integration environment

| Key | Type/default | Effect |
|---|---|---|
| `HOOKS_ADAPTIVE_SEGMENTS_READY` | directory; `$HOME/.hooks-adaptive/telemetry-segments/ready` | Tama immutable closed-segment inbox. When absent, legacy telemetry logs are considered. |
| `OKO_CLI` | executable path/name; search `oko-cli` on `PATH` | Used by `oko-refresh` and `rebuild-oko --reindex`. |
| `TRANSCRIPT_LAKE_SQL` | directory; embedded SQL | Overrides both compiled `views.sql` and `signals.sql` for operator development. If set, the requested file must exist; there is no fallback per missing file. |
| `TRANSCRIPT_LAKE_GOAL_LLAMA_CLI` | file; search `llama-cli` | First-choice local goal runtime executable. Must name a file. |
| `JEDEN_GOAL_LLAMA_CLI` | file; unset | Compatibility fallback for the same runtime, checked after `TRANSCRIPT_LAKE_GOAL_LLAMA_CLI`. |
| `TRANSCRIPT_LAKE_GOAL_MODEL` | GGUF file; qualified shared/cache artifact | Explicit goal model. It must pass the pinned SHA-256 validation. |

`goal title`/`goal label` may download pinned model/prompt artifacts from the compiled Hugging Face revision when no valid shared or cached artifact exists. Core streaming makes no network request.

## State layout

| Path under `LAKE_DATA` | Role | Authority |
|---|---|---|
| `events/runtime=<r>/date=<d>/part-<hash>.ndjson` | masked canonical events | authoritative |
| `cursors.json` | per-source `{mtimeMs,size,offset}` | authoritative resume metadata |
| `stream.lock/owner.json` | live writer lease | ephemeral coordination |
| `cursors.lock` | cursor publication lock | ephemeral coordination |
| `stream-status.json` | last/live stream summary | operational state |
| `labels/labels.ndjson` | operator annotations | independent operator state |
| `exports/oko/` | per-session Oko projection and `export-cursors.json` | rebuildable |
| `staging/oko-export/` | bounded full-export staging | temporary |
| `parquet/runtime=<r>/events.parquet` | analytical mirror | rebuildable |
| `models/jeden-goal-qwen3-4b/<sha>/` | pinned goal artifacts | cache |

## Flag types, defaults, and bounds

- Common `--json`: boolean, false.
- Common bounded `--limit`: integer, default 20, range 1–500.
- `show --limit`: default 2,000, range 1–50,000.
- `stats --days`: default 7, range 1–500.
- Runtime/source values: one of `claude`, `codex`, `omp`, `droid`, `kimi`, `hooks`.
- `show --include`: comma-separated event types; default `user,assistant`; `all` expands to all seven.
- `signals --report`: `freshness` (default), `frustration`, `overlap`, or `daily`.
- `clean --target`: `all` (default), `parquet`, or `oko`; preview unless `--apply`.
- `label add --source`: `manual` default, or `manual|human|model|brama` with optional `:<detail>` matching `[A-Za-z0-9._/-]+`.
- `label add --aspect`: required non-empty string, trimmed and lowercased.
- `label add --value`: required non-empty trimmed string.

Long flags are exact, unknown flags fail, duplicate flags fail, and a value flag never consumes another `--flag`. See [CLI reference](cli-reference.md) for each command's accepted set.