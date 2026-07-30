# Resume an incremental ingest

1. **Goal:** Import only complete bytes appended since the last committed checkpoints.
2. **Status:** Development `0.x`; implemented capability, execution evidence pending.
3. **Risk:** Local mutation and provider-facing read.
4. **Environment:** macOS terminal with installed Transcript Lake and one existing valid Lake.
5. **Preconditions:** Prior successful ingest; no concurrent writer; vendor transcript changes are normal appends, not rewrites.
6. **Inputs:** Existing `LAKE_DATA`; optional newly appended local agent conversations.
7. **Artifacts and side effects:** Appends masked events to daily NDJSON partitions, atomically advances source cursors after flushed batches, refreshes last-ingest summary, and incrementally refreshes the derived Oko export. Vendor files remain unchanged.
8. **Steps:**

```sh
export LAKE_DATA="/absolute/operator-owned/lake"
transcript-lake status
transcript-lake ingest
transcript-lake status
```

9. **Verification:** Ingest emits a single JSON summary. `partial` is false and `failures` is zero for a complete run. `events` counts only newly accepted events; an unchanged rerun reports no new events. Final cursor freshness and last-ingest time advance while unchanged partitions remain append-only.
10. **Failure path:** A source truncation, same-size rewrite, invalid segment, parser failure, or unreadable file makes the run partial and exits non-zero. Already flushed checkpoints remain durable. Preserve the error and source, then follow [rebuild into an empty root](../recovery/rebuild-into-empty-root.md) for non-append source changes.
11. **Cleanup or off-switch:** No cleanup is required. Stop invoking ingest to pause collection. Retention is operator-owned; do not delete cursors independently of partitions.
12. **Next:** [Query sessions](query-sessions.md), [compact to Parquet](../operations/compact-to-parquet.md), or [export for Oko](../integrations/oko/export-for-oko.md).
