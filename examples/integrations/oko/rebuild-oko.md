# Reconstruct the Oko projection

1. **Goal:** Rebuild masked per-session JSONL from authoritative Lake partitions after projection loss or damage.
2. **Risk:** Derived local mutation. Oko is not invoked unless `--reindex` is supplied.
3. **Environment:** macOS terminal and valid Lake partitions. Oko installation is optional.
4. **Preconditions:** No stream commit or other writer in flight; enough disk for one staged per-session copy.
5. **Inputs:** `LAKE_DATA`; reconstruction always covers the complete authoritative Lake.
6. **Artifacts and side effects:** Rebuilds `exports/oko/runtime=<runtime>/<session-hash>.jsonl` through staging, atomically replaces projection metadata, and prunes derived files absent from authoritative partitions.
7. **Steps:**

```sh
LAKE="/absolute/operator-owned/lake"
transcript-lake --data-dir "$LAKE" rebuild-oko
transcript-lake --data-dir "$LAKE" paths
```

8. **Observable result:** The summary reports `mode: full`, materialized session and record counts, malformed input count, and pruned derived files. Rows declare `lake_schema: oko-import-v1` and deterministic event UUIDs.
9. **Failure path:** A malformed Lake row aborts reconstruction, removes staging, and preserves the prior projection. Authoritative NDJSON and source cursors are never changed.
10. **Off-switch:** The live stream already maintains this projection. After disconnecting Oko and other readers, `clean --target oko` previews derived removal; `--apply` performs it.
11. **Related operation:** [Reindex Oko](reindex-oko.md).
