# Query cross-source signals with Oko

1. **Goal:** Join masked Lake events with the local Oko transcript index to inspect hook decisions and frustration signals.
2. **Status:** Development `0.x`; optional integration, execution evidence pending.
3. **Risk:** Read-only, external DuckDB process. Fresh DuckDB installations may fetch the SQLite extension; run offline only when the extension is already installed.
4. **Environment:** macOS, DuckDB CLI `1.5.x`, valid Lake, compatible local Oko index at its documented default path.
5. **Preconditions:** Review `sql/signals.sql`; confirm Oko's index is not being migrated; decide whether network extension installation is acceptable.
6. **Inputs:** `LAKE_DATA`, Lake views, and Oko's read-only SQLite index.
7. **Artifacts and side effects:** Reads Lake and attaches Oko read-only. DuckDB may install/cache its SQLite extension outside `LAKE_DATA`. No Oko rows are modified.
8. **Steps:**

```sh
export LAKE_DATA="/absolute/operator-owned/lake"
duckdb -c "SET VARIABLE lake_data = '$LAKE_DATA';" -c ".read sql/views.sql" -c ".read sql/signals.sql" -c "SELECT * FROM blocks_by_hook ORDER BY blocks DESC"
```

Run this command from an exact Transcript Lake source checkout matching the installed version because it names packaged SQL assets directly.

9. **Verification:** The query returns zero or more grouped hook rows with block counts and session counts. `oko_freshness` is available when the index attaches. Lake-only views remain queryable even if no matching Oko rows exist.
10. **Failure path:** Missing Oko index causes only statements tagged `REQUIRES-OKO` to fail; Lake-only views remain valid. Missing SQLite extension or denied network access is a named DuckDB dependency failure, not authorization to retry indefinitely.
11. **Cleanup or off-switch:** Close DuckDB. Remove its cached extension only according to DuckDB's own documentation; Transcript Lake owns no external cache cleanup.
12. **Next:** Inspect [Oko export](../oko/export-for-oko.md) freshness if the index does not reflect expected Lake sessions.
