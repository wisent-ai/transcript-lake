# Export canonical sessions for Oko

1. **Goal:** Materialize masked per-session JSONL for every runtime supported by Transcript Lake.
2. **Status:** Development `0.x`; implemented integration, execution evidence pending.
3. **Risk:** Derived local mutation. Oko is not invoked in this example.
4. **Environment:** macOS terminal and valid Lake partitions. Oko installation is optional.
5. **Preconditions:** No concurrent export writer; enough disk for one rebuildable per-session copy.
6. **Inputs:** `LAKE_DATA`; optional `--full` only when a complete rebuild is intended.
7. **Artifacts and side effects:** Writes `LAKE_DATA/exports/oko/runtime=<runtime>/<session-hash>.jsonl` and atomic export cursors. Incremental mode preserves unchanged file mtimes. Full mode stages all sessions, then prunes derived files absent from complete input.
8. **Steps:**

```sh
export LAKE_DATA="/absolute/operator-owned/lake"
transcript-lake export-oko
transcript-lake export-oko
transcript-lake export-oko --full
```

9. **Verification:** Each command emits one JSON summary. The first reports incremental writes for changed sessions. The unchanged second run reports no rewritten session files. Full mode reports `mode: full`, all materialized session/record counts, and pruned derived files. Rows declare `lake_schema: oko-import-v1` and deterministic event UUIDs.
10. **Failure path:** A malformed Lake row aborts before advancing export cursors; full mode removes its staging tree and does not prune the prior export. Source partition replacement forces full mode rather than unsafe tail import. Preserve authoritative NDJSON and diagnostic output.
11. **Cleanup or off-switch:** Stop exporting. After disconnecting Oko and other readers, delete only `LAKE_DATA/exports/oko`; rerunning full export rebuilds it.
12. **Next:** [Reindex Oko](reindex-oko.md) or inspect files through Oko's configured peer root.
