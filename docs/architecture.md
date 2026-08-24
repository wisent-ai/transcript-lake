# Architecture

## Data flow

```text
vendor stores / Tama closed segments (read-only)
                |
          runtime adapter
                | RawEvent (unmasked, in memory)
                v
       canonicalizer + masker + caps
                |
        CanonicalEvent (masked)
          /          |          \
 daily NDJSON   Oko projection   cursor checkpoint
(authoritative) (rebuildable)    (resume state)
          |
 compiled DuckDB views
     /        |       \
read CLI   Parquet   operator labels
           mirror    (separate store)
```

## Components

### Adapter layer

`src/adapters/` implements one pure line parser for Claude, Codex, omp, Factory Droid, and Kimi. `src/hook_segments.rs` ingests Tama closed segments or the legacy hook log. Adapters own vendor-field interpretation, not masking or IO. Malformed/incomplete lines yield no events because a live transcript commonly ends with a torn line.

### Stream layer

`src/commands/stream.rs` installs recursive native filesystem watches before catch-up, so appends during initialization are queued. `src/stream.rs` discovers cursors, reads only complete appended lines, batches up to 512 canonical events, and maps each source/date to a deterministic partition.

The stream status file moves through `catching-up`, `running`, `degraded`, and final stopped state. Human logs use `<timestamp> stream <kind> key=value`; `--json` emits one object per log with `ts`, `kind`, and detail fields.

### State and leases

`LAKE_DATA/cursors.json` maps source paths to `mtimeMs`, `size`, and `offset`. Publication writes `.cursors.json.tmp-<pid>-<uuid>`, fsyncs it, renames it, then fsyncs the data directory under `cursors.lock`.

`stream.lock/owner.json` identifies one writer by host, pid, start time, and random token. An atomic prepared-directory rename claims the lease. Live owners cause an immediate refusal. Dead owners may be removed after liveness checks. Release verifies the token before removal.

### Authoritative partitions

Canonical events are append-only NDJSON at:

```text
LAKE_DATA/events/runtime=<runtime>/date=<YYYY-MM-DD>/part-<12-hex-path-digest>.ndjson
```

The event date comes from the timestamp prefix or `unknown`. NDJSON partitions are the canonical masked store. Parquet and Oko exports are derived and may be deleted/rebuilt.

### Oko projection

Conversation events are projected to:

```text
LAKE_DATA/exports/oko/runtime=<runtime>/<sha256(runtime + newline + session-id)>.jsonl
```

Each `oko-import-v1` line includes a deterministic 32-hex UUID and the canonical conversation fields. Session files are deduplicated by UUID and ordered by timestamp then UUID. `export-cursors.json` records each partition's `size`, `mtimeMs`, and `physicalSize` after successful publication. Truncation, same-size rewrite, first run, or `rebuild-oko` selects a full rebuild through bounded staging; ordinary appends are incremental. Oko scans this tree read-only.

### Read model

The binary embeds `sql/views.sql` and `sql/signals.sql`. Core views are `events`, `sessions`, `interrupted_sessions`, `tools_daily`, `tokens_daily`, `hook_decisions`, `blocks_by_hook`, and `labels`. Signal views attach Oko's SQLite catalogue only when requested. The CLI invokes DuckDB; streaming itself has no DuckDB dependency.

### Labels

`LAKE_DATA/labels/labels.ndjson` is an append-only operator annotation store with `ts`, `session_id`, `runtime`, `aspect`, `value`, `note`, and `source`. It is neither authoritative transcript evidence nor masked by the event masker. It does not share the event writer lease.

## Failure containment

- Vendor stores are always read-only.
- Invalid cursor state is refused rather than silently reset.
- Source shrink or rewrite is refused rather than merged.
- Recovery writes only to a different empty root.
- Malformed Oko rebuild rows refuse cursor advancement and do not modify authoritative partitions.
- `clean` targets only derived Parquet/Oko state and previews unless `--apply` is present.

See the [ingestion reference](ingestion-reference.md), [configuration](configuration.md), and normative [data contract](LAKE.md).