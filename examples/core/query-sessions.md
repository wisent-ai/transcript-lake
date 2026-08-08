# Query cross-runtime sessions

1. **Goal:** Retrieve recent masked session summaries across every ingested runtime.
2. **Status:** Development `0.x`; implemented capability, execution evidence pending.
3. **Risk:** Read-only for the documented query; external DuckDB process.
4. **Environment:** macOS terminal, valid Lake partitions, DuckDB CLI `1.5.x` on `PATH`.
5. **Preconditions:** At least one ingested session for a non-empty result.
6. **Inputs:** Selected Lake, optional runtime/project filters, and a bounded limit.
7. **Artifacts and side effects:** Transcript Lake writes nothing. DuckDB reads NDJSON through the pinned `sessions` view and formats rows or JSON.
8. **Steps:**

```sh
LAKE="/absolute/operator-owned/lake"
transcript-lake --data-dir "$LAKE" sessions --limit 20
transcript-lake --data-dir "$LAKE" sessions --runtime codex --project wisent --limit 20 --json
transcript-lake --data-dir "$LAKE" query --json "SELECT * FROM tokens_daily ORDER BY day DESC LIMIT 20"
```

9. **Verification:** `sessions` exposes runtime-native identity, project, message/tool counts, token counters, and span. Runtime is part of identity, so equal native IDs from different providers remain separate. `query` remains available for documented or operator-owned advanced SQL.
10. **Failure path:** Missing DuckDB names the unavailable dependency and exits non-zero. Invalid runtime or out-of-range limit fails before DuckDB starts or Lake state changes.
11. **Cleanup or off-switch:** None for this read-only query. Remove any shell redirection output only if the operator created it and no longer needs it.
12. **Next:** Run a documented view such as `tokens_daily` or use [cross-source signals](../integrations/duckdb/cross-source-signals.md).
