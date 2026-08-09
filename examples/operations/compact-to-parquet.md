# Compact partitions to Parquet

1. **Goal:** Create faster-scanning Parquet mirrors without replacing authoritative NDJSON evidence.
2. **Risk:** Derived local mutation and external DuckDB process.
3. **Environment:** macOS terminal, valid Lake, DuckDB CLI `1.5.x`.
4. **Preconditions:** Sufficient free disk for a second representation; no other compaction writes the same derived directory.
5. **Inputs:** Existing runtime/date NDJSON partitions under `LAKE_DATA/events`.
6. **Artifacts and side effects:** Rebuilds one Parquet file per runtime under `LAKE_DATA/parquet/runtime=<runtime>`. Reports source and output sizes and never deletes NDJSON.
7. **Steps:**

```sh
LAKE="/absolute/operator-owned/lake"
transcript-lake --data-dir "$LAKE" status
transcript-lake --data-dir "$LAKE" compact --source codex --json
transcript-lake --data-dir "$LAKE" paths
transcript-lake --data-dir "$LAKE" clean --target parquet
```

8. **Observable result:** The report lists each runtime with source partitions, output path, source bytes, and Parquet bytes. Authoritative partitions remain unchanged.
9. **Failure path:** Missing DuckDB, insufficient disk, malformed source rows, or output failure exits non-zero and preserves NDJSON.
10. **Cleanup:** `clean --target parquet` previews path and bytes; `--apply` removes only derived Parquet.
11. **Related operation:** Use DuckDB directly against the derived files or [query canonical views](../core/query-sessions.md).
