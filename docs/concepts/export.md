# Concept: export and derived data

An export is a rebuildable representation of authoritative NDJSON partitions.

## Oko export

`LAKE_DATA/exports/oko/runtime=<runtime>/<session-hash>.jsonl` groups conversation events per session using the `oko-import-v1` line schema. Deterministic event UUIDs make replays deduplicable; rows sort by timestamp then UUID. `export-cursors.json` tracks partition size, physical size, and mtime for incremental refresh.

Oko reads this projection. Oko does not own it, write the Lake, or replace masking. `rebuild-oko` reconstructs it; `oko-refresh` asks Oko to reindex the current export.

## Parquet

`compact` writes `LAKE_DATA/parquet/runtime=<runtime>/events.parquet` through DuckDB. It is an analytical mirror, not a transaction log.

`clean --target parquet|oko|all` previews derived removal. `--apply` removes only selected Parquet/Oko trees; it never removes canonical events, cursors, or labels.

Derived artifacts can be stale and may be deleted/rebuilt. Canonical partitions cannot.

See [architecture](../architecture.md#oko-projection) and [CLI reference](../cli-reference.md#derived-data-and-oko).