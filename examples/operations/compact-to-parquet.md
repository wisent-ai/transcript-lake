# Compact partitions to Parquet

1. **Goal:** Create faster-scanning Parquet mirrors without replacing authoritative NDJSON evidence.
2. **Status:** Development `0.x`; implemented capability, execution evidence pending.
3. **Risk:** Derived local mutation and external DuckDB process.
4. **Environment:** macOS terminal, valid Lake, DuckDB CLI `1.5.x`.
5. **Preconditions:** Sufficient free disk space for a second representation of all selected partitions; no other compaction writing the same derived directory.
6. **Inputs:** Existing runtime/date NDJSON partitions under `LAKE_DATA/events`.
7. **Artifacts and side effects:** Rebuilds one Parquet file per runtime under `LAKE_DATA/parquet/runtime=<runtime>`. Reports source and output byte sizes. Never deletes NDJSON.
8. **Steps:**

```sh
LAKE="/absolute/operator-owned/lake"
transcript-lake --data-dir "$LAKE" status
transcript-lake --data-dir "$LAKE" compact --source codex --json
transcript-lake --data-dir "$LAKE" paths
transcript-lake --data-dir "$LAKE" clean --target parquet
```

9. **Verification:** The compact report lists each runtime that had source partitions, output path, source bytes, and Parquet bytes. Status continues to report the same authoritative partitions and now reports derived Parquet inventory. Queries over NDJSON remain available.
10. **Failure path:** Missing DuckDB, insufficient disk, malformed source rows, or output write failure exits non-zero. Preserve NDJSON. Remove only incomplete derived output after confirming no reader is using it, then correct the dependency or capacity issue.
11. **Cleanup or off-switch:** `clean --target parquet` previews path and bytes. After downstream readers stop, add `--apply` to remove only derived Parquet. Authoritative `events` and `cursors.json` remain.
12. **Next:** Use DuckDB directly against the derived files or [query canonical views](../core/query-sessions.md).
