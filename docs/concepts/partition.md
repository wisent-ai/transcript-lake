# Concept: partition

A partition is an append-only daily NDJSON file of canonical masked events:

```text
LAKE_DATA/events/runtime=<runtime>/date=<YYYY-MM-DD>/part-<12-hex-path-digest>.ndjson
```

Runtime and event-date directories make pruning and DuckDB scans predictable. The filename digest is derived from the full source path, so one source consistently lands in the same file for a runtime/day while the plaintext path is not encoded in the partition name. Events without a usable date use `date=unknown`.

Partitions are authoritative. Parquet mirrors and Oko per-session exports are derived. Never edit, truncate, merge, or replace a live partition by hand; preserve the root and rebuild from vendor-owned sources to a different empty destination.

DuckDB readers use an explicit schema and tolerate a torn final line during concurrent append. The stream writes complete batches before publishing their source cursor.

See [cursor](cursor.md), [architecture](../architecture.md), and [recovery runbook](../runbook.md#source-shrank-or-changed-without-an-append).