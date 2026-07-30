# Query cross-runtime sessions

1. **Goal:** Retrieve recent masked session summaries across every ingested runtime.
2. **Status:** Development `0.x`; implemented capability, execution evidence pending.
3. **Risk:** Read-only for the documented query; external DuckDB process.
4. **Environment:** macOS terminal, valid Lake partitions, DuckDB CLI `1.5.x` on `PATH`.
5. **Preconditions:** At least one ingested session for a non-empty result. Review SQL before execution because arbitrary DuckDB SQL is not sandboxed by Transcript Lake.
6. **Inputs:** `LAKE_DATA` and a quoted SQL statement over the documented `sessions` view.
7. **Artifacts and side effects:** Transcript Lake itself writes nothing. DuckDB reads NDJSON through the pinned views. The command emits DuckDB-formatted rows to stdout.
8. **Steps:**

```sh
export LAKE_DATA="/absolute/operator-owned/lake"
transcript-lake query "SELECT runtime, session_id, project, events, last_ts FROM sessions ORDER BY last_ts DESC LIMIT 20"
```

9. **Verification:** Rows expose runtime-native session identity, project, event count, and latest timestamp. The result may be empty but must preserve the stated columns. Text remains the masked Lake representation; this view does not reconstruct raw vendor files.
10. **Failure path:** Missing DuckDB names the unavailable dependency and exits non-zero. Invalid SQL reports DuckDB's error and leaves Lake files unchanged. A missing `sql/views.sql` indicates a broken installation and must not fall back to ad hoc schema inference.
11. **Cleanup or off-switch:** None for this read-only query. Remove any shell redirection output only if the operator created it and no longer needs it.
12. **Next:** Run a documented view such as `tokens_daily` or use [cross-source signals](../integrations/duckdb/cross-source-signals.md).
