# Transcript Lake

A single, privacy-masked, real-time archive of the coding-agent conversations
on this machine, queryable with plain SQL through DuckDB.

- Code lives in this repository (`transcript-lake`).
- Data lives outside the repository, in the directory named by the `LAKE_DATA`
  environment variable or, when that is unset, `~/.transcript-lake`.
  Partitions, cursors, projections, and stream state all live there; nothing is
  written into the repository.
- The DuckDB CLI (version one dot five) must be on `PATH` as `duckdb`.
- The CLI is one Rust binary. It carries its own view definitions and needs no
  language runtime, package manager, or interpreter on the host.

## Canonical event

Every source line from every runtime is normalised into one JSON object per
line (NDJSON) with this frozen shape:

| Field       | Type            | Meaning                                                       |
| ----------- | --------------- | ------------------------------------------------------------- |
| ts          | string          | ISO-formatted UTC timestamp of the event                      |
| runtime     | string          | one of claude, codex, omp, droid, kimi, hooks                 |
| machine     | string          | `os.hostname()` of the machine that recorded the event        |
| session_id  | string          | conversation identity as the source runtime names it          |
| project     | string or null  | absolute working directory, best effort                       |
| event_type  | string          | user, assistant, thinking, tool_call, tool_result, meta, or hook_decision |
| text        | string          | masked body text; may be empty                                |
| tool_name   | string or null  | tool identity for tool events; hook identity for hook events  |
| model       | string or null  | model name when the runtime reports one                       |
| tokens_in   | number or null  | prompt-side usage counter reported by the runtime             |
| tokens_out  | number or null  | completion-side usage counter reported by the runtime         |
| extra       | object          | small masked payload of source-specific leftovers             |

The `extra` object stays small by design: full tool outputs are never
embedded, and any single text field is capped at sixty-four kilobytes.

The driver also stores `extra.source_stem_hash`, a one-way digest of the
source filename stem. The plaintext stem never enters lake storage. This
bridges indexes that name a conversation by file to the runtime-native
`session_id` without persisting the external identifier.

## Adapters

Each runtime has one module in `src/adapters/` implementing the frozen
`Adapter` trait: `runtime()` returns the stable identifier, `roots(home)`
returns watched directories, `list_sessions(root)` supports historical replay,
`entry_for(root, path)` resolves a filesystem notification without scanning
siblings, and `parser(ctx)` returns a stateful per-file `Parser` with `on_line`
and `end`. Parsers swallow malformed lines without failing the process, never
perform IO inside `on_line`, and emit UNMASKED text: masking is the stream
driver's job, so adapters stay pure.

Source formats, as verified on this machine:

- **claude** — `~/.claude/projects/<encoded-cwd>/<id>.jsonl`. Typed lines
  carrying message content blocks (text, thinking, tool use, tool result)
  plus per-message usage counters. Oversized tool results arrive as file
  references; the reference path is recorded in `extra` and never followed.
- **codex** — `~/.codex/sessions/<year>/<month>/<day>/rollout-*.jsonl`.
  Envelope lines `{timestamp, type, payload}` covering session metadata,
  turn context, user and agent messages, usage counts, and response items
  (message, reasoning, custom tool call, custom tool call output).
- **omp** — `~/.omp/agent/sessions/<encoded-cwd>/<stamp>_<id>.jsonl`. Typed
  lines: session header, message (role plus content blocks with text and tool
  parts), compaction, custom message, model change. Non-JSONL artifacts in
  the same tree are skipped.
- **droid** — `~/.factory/sessions/<encoded-cwd>/<uuid>.jsonl` with
  `<uuid>.settings.json` sidecars. The first line is the session start
  record; sidecars contribute meta events only.
- **kimi** — `~/.kimi-code/sessions/wd_*/session_*/agents/main/wire.jsonl`
  with `state.json` nearby for identity. Wire records are typed (metadata,
  config updates, message and tool records); config and system-prompt
  records contribute at most meta events and never text.
- **hooks** — Tama publishes immutable closed segments in its ready directory;
  each segment is streamed once, content-checked, committed, and acknowledged.
  When that directory is absent, the adapter follows legacy
  `~/.hooks-adaptive/telemetry.jsonl` and `telemetry.prev.jsonl`. Each record
  becomes a `hook_decision` event: `tool_name` is the hook identity, `text` is
  the masked reason, and `extra` carries decision, event, and infrastructure.

## Stream, cursors, partitions

`src/commands/stream.rs` owns the long-running process. It recursively watches
every adapter root and the Tama ready directory. Each notification carries the
changed path through a short coalescing window to `src/stream.rs`, which asks
the owning adapter for one session entry and reads only bytes appended since
that file's newline-aligned cursor.

For each source delta, one writer lease covers canonical partition appends,
affected Oko session projection writes, and the cursor checkpoint. The cursor
advances only after both durable outputs succeed, so restart never skips source
bytes and readers never observe a cursor ahead of its evidence.

- Cursor store: one JSON file, `LAKE_DATA/cursors.json`, mapping each source
  file to `{mtimeMs, size, offset}`. Writes are atomic (temporary file plus
  rename). Unreadable or structurally invalid cursor state is a hard failure:
  silently restarting would append duplicate evidence to existing partitions.
- Source replacement: truncation or a same-size rewrite after a complete
  checkpoint is rejected with recovery guidance. Supported vendor files are
  append-only between checkpoints.
- Partition path:
  `LAKE_DATA/events/runtime=<runtime>/date=<year>-<month>-<day>/part-<hash>.ndjson`,
  append mode, where `<hash>` is the first twelve hex characters of a SHA
  digest of the source file path. One source file therefore always lands in
  the same partition file per day.
- Concurrency: one short writer lease owns a source delta. Compaction, recovery,
  or another writer fails before mutation rather than waiting or duplicating
  appends.
- Historical recovery: `rebuild --to <empty-path> [--source <runtime>]` replays
  selected source history into a separate empty Lake; live streaming never
  performs a full-root scan.

## Masking guarantees

`src/redact.rs` exposes `Masker`; the stream uses one masker instance while it
canonicalizes every event in a source delta.

- Each hit is replaced ENTIRELY by a marker of the form
  `[masked:<class>:<length>:<fingerprint>]`. No plaintext prefix or suffix of
  the original value survives.
- `<class>` is one of `token` (provider-credential-shaped strings: a short
  lowercase prefix of two to seven letters, a dash, then twenty or more
  characters from the letter, digit, underscore, dash alphabet), `entropy`
  (dense runs of forty or more base-sixty-four-like characters), or
  `assignment` (an uppercase NAME, an equals sign, then a long value).
- `<length>` is the original length, so downstream analysis can still reason
  about size without seeing content.
- `<fingerprint>` is the first eight hex characters of a SHA digest of the
  value: stable enough to correlate reuse of the same value across events,
  and non-reversible.
- The transform is pure, deterministic, and idempotent — masking a masked
  string changes nothing.
- Masking happens after parsing and before any byte reaches a partition or Oko
  projection file, so no unmasked text is written to `LAKE_DATA`.
- Per-class hit counts are emitted with each successful stream commit.

## Querying with DuckDB

`transcript-lake query "<sql>"` runs DuckDB with the selected data root,
loads the canonical views, then runs operator SQL. `sessions`, `events`,
`search`, `stats`, and `hooks` expose bounded common queries without
requiring SQL input. `search` runs a case-insensitive substring match over
the `text` column of the canonical `events` view and escapes LIKE
wildcards in the operator's term, so the term always matches literally.

The view definitions live in `sql/views.sql` and `sql/signals.sql` in this
repository and are compiled into the binary at build time, so a moved or
partially copied installation can never present a Lake without its views.
`TRANSCRIPT_LAKE_SQL` names a directory of replacement scripts for iterating
on view definitions against an installed binary; a directory that is set but
missing the requested script is an error rather than a silent fall back to the
compiled copy.

Views defined by `views.sql`:

- `events` — every canonical column plus `filename` (the partition file the
  row came from). The reader pins the frozen column list explicitly, so
  schema inference can never drift, and skips a torn final line while an
  stream appends.
- `sessions` — one row per runtime-native conversation identity: runtime,
  project, first and last timestamps, message and tool counts, summed usage
  counters, and `oko_session_hash`, the aggregate one-way source-stem alias.
- `interrupted_sessions` — conversations that stopped without an answer:
  the last recorded turn is a user message the agent never replied to
  (`stopped_as = 'unanswered'`) or a tool call whose run was cut off before
  the agent spoke again (`stopped_as = 'cut_off_mid_tool'`). Carries the
  session view's identity, span, and counts plus `last_user_text`, the masked
  opening of that final request. `transcript-lake sessions --interrupted`
  reads it, newest first.
- `tools_daily` — tool-call volume per day, runtime, and tool.
- `tokens_daily` — usage counters summed per day, runtime, and model.
- `hook_decisions` — the adaptive-hook decision stream (runtime `hooks`)
  with decision, hook event, infra flag, and masked reason.
- `blocks_by_hook` — blocking pressure per hook: block count, distinct
  conversations, first and last block, and the most recent reason.
- `labels` — operator-owned session annotations from `LAKE_DATA/labels`:
  one row per assignment (`ts`, `session_id`, `runtime`, `aspect`, `value`,
  `note`, `source`). The store is append-only and this view exposes the full
  history; CLI reads apply latest-assignment-wins per session and aspect.
  The same empty-store stub and torn-final-line tolerance as the events
  view apply.

Empty-lake bootstrap: DuckDB refuses to bind a view over a glob that matches
no files, so `views.sql` materialises a zero-row stub file under `/tmp` and
points the reader at it only while the lake has no partitions at all. As soon
as one partition exists, the views read the live glob directly, and partition
files created later are visible to every subsequent query without reloading.

`signals.sql` installs and loads DuckDB's SQLite extension, attaches the
Oko transcript index read-only, and creates named cross-source views:
`oko_frustration`, `hook_frustration_overlap`, `hook_frustration_daily`, and
`oko_lake_freshness`. `transcript-lake signals --report <name>` selects those
views through the installed CLI, so examples never depend on source-tree SQL.
The join tries native session identity, then compares the source-stem digest
with a digest of Oko's session key; plaintext aliases are never persisted.
Statements touching the `oko` schema are tagged `REQUIRES-OKO` and fail when
the index is absent without affecting Lake-only commands.

`compact` converts each runtime partition directory to Parquet under
`LAKE_DATA/parquet/runtime=<r>/` and reports both sizes. The NDJSON originals
are never deleted; Parquet is an additive, faster-scan mirror.

### Oko import path

Transcript Lake is the only component that parses local vendor transcript
stores for Oko's historical catalog. `src/stream.rs` projects each newly
canonicalized event into the affected conversation file in the same source
transaction under:

`LAKE_DATA/exports/oko/runtime=<runtime>/<session-hash>.jsonl`

Oko recursively imports that directory, decodes the canonical rows, and
maintains its own single-writer SQLite FTS index as a disposable interactive
read model. DuckDB continues to own cross-runtime analytics. Oko may still
inspect native process/session locations to launch or resume a live agent,
but it does not parse them into the historical index.

The exporter covers every conversation runtime present in the lake and keeps
provider identity in the directory name. User, assistant, thinking, tool call,
tool result, and meta events retain the frozen Lake fields plus a deterministic
event UUID.

- Live commits merge only the newly canonicalized rows into affected session
  files; the stream does not reread Lake partitions to produce them.
- `rebuild-oko` is the recovery path: it scans authoritative partitions,
  rebuilds every session in bounded memory, and prunes files absent from the
  completed Lake scan.
- Replayed Lake events are deduplicated by UUID before publication.
- Session files and projection metadata are written atomically. Unchanged files
  keep their mtimes, so Oko's indexer skips them.
- `--reindex` optionally invokes `oko-cli transcripts reindex`; a running Oko
  also discovers projection changes through its own filesystem observation.
- `freshness()` compares the Oko index read-only with Lake cursor recency.

## Operator labels

`transcript-lake label` records operator-owned annotations over sessions:
`label add` appends one JSON record per assignment to
`LAKE_DATA/labels/labels.ndjson` after validating the session against the
canonical `sessions` view, `label list` shows the latest assignment per
session and aspect (newest first), and `label aspects` aggregates the
effective labels. A record carries `ts`, `session_id`, `runtime`
(denormalized from the session row), `aspect` (lowercase-normalized),
`value`, nullable `note`, and namespaced provenance `source` matching
`^(manual|human|model|brama)(:[A-Za-z0-9._/-]+)?$` (`manual` when `--source`
is absent). `manual` and `human` mean a person assigned the label;
`brama:<model-id>` means an LLM assigned it through the Brama gateway;
`model:<artifact>` means a local fine-tuned classifier merely suggested it.
Training ground truth is `manual`/`human`/`brama:*`; `model:*` suggestions
await acceptance and are excluded from training runs by convention — that
filter lives in transcript-label-trainer, not in this store.

- Labels are derived operator data, not masked Lake events: label text is
  stored exactly as given and never passes through the masker, so labels
  must not carry secrets.
- The store is append-only. Re-labeling the same session and aspect adds
  another record; the latest assignment wins in `label list` and
  `label aspects`, while the `labels` DuckDB view exposes the full history
  for joins against `sessions` and `events`.
- Writes are single complete lines appended in one call and fsynced; a
  crash loses at most the record being written, and readers skip a torn
  final line, mirroring the events partitions.
- The events writer lease is deliberately not taken: labels live outside
  `events/` and `cursors.json`, so labeling neither blocks nor is blocked
  by the stream.
- Deleting `LAKE_DATA/labels/` loses only labels; events, cursors, and
  exports are unaffected.

## Out of scope, deliberately

- **gemini and qwen** — no session stores for these runtimes exist on this
  machine, so no adapters ship. Adding one later means dropping a module
  into `src/adapters/` implementing the frozen `Adapter` trait; nothing
  else changes.
- **Semantic embeddings** — vector search over the lake waits on a local
  embedding backend; until then the lake offers lexical matching through
  `search` and SQL, and the Oko index covers full-text search needs.
- **Implicit corpus backfill** — the live stream never scans full history.
  Historical reconstruction is an explicit separate-root `rebuild`.

## Operating instructions

```sh
LAKE="$HOME/.transcript-lake"

transcript-lake --data-dir "$LAKE" paths
transcript-lake --data-dir "$LAKE" sources
transcript-lake --data-dir "$LAKE" doctor
transcript-lake --data-dir "$LAKE" stream
transcript-lake --data-dir "$LAKE" sessions --limit 20
transcript-lake --data-dir "$LAKE" events --type tool_call --limit 20
transcript-lake --data-dir "$LAKE" search "ssh" --limit 20
transcript-lake --data-dir "$LAKE" label add <session-id> --aspect reviewed --value yes
transcript-lake --data-dir "$LAKE" label list --aspect reviewed
transcript-lake --data-dir "$LAKE" stats --days 7
transcript-lake --data-dir "$LAKE" hooks --decision block
transcript-lake --data-dir "$LAKE" signals --report freshness
transcript-lake --data-dir "$LAKE" query "FROM tokens_daily ORDER BY day DESC"
transcript-lake --data-dir "$LAKE" compact --source droid
transcript-lake --data-dir "$LAKE" rebuild-oko --reindex
transcript-lake --data-dir "$LAKE" clean --target all
```

For recovery, preserve the current root and use
`transcript-lake --data-dir "$LAKE" rebuild --to <empty-path>`. Applied
`clean` removes only rebuildable Parquet/Oko data and shares the state writer
lease with streaming, export recovery, and compaction.

The stream is restart-safe: cursors make every source tail incremental, masking
is idempotent, and partitions remain append-only per source file and day.
