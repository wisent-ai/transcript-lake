# Query cross-source signals with Oko

1. **Goal:** Join masked Lake events with the local Oko transcript index to inspect hook decisions and frustration signals.
2. **Status:** Development `0.x`; optional integration, execution evidence pending.
3. **Risk:** Read-only, external DuckDB process. Fresh DuckDB installations may fetch the SQLite extension; run offline only when the extension is already installed.
4. **Environment:** macOS, DuckDB CLI `1.5.x`, valid Lake, compatible local Oko index at its documented default path.
5. **Preconditions:** Confirm Oko's index is not being migrated; decide whether network extension installation is acceptable.
6. **Inputs:** Selected Lake, Oko's read-only SQLite index, and one named report: `frustration`, `overlap`, `daily`, or `freshness`.
7. **Artifacts and side effects:** The CLI loads its compiled-in, version-matched SQL, reads Lake, and attaches Oko read-only. DuckDB may install/cache its SQLite extension outside the Lake. No Oko rows are modified.
8. **Steps:**

```sh
LAKE="/absolute/operator-owned/lake"
transcript-lake --data-dir "$LAKE" signals --report freshness
transcript-lake --data-dir "$LAKE" signals --report frustration --limit 20
transcript-lake --data-dir "$LAKE" signals --report overlap --json
```

9. **Verification:** Freshness compares the Oko index with each Lake runtime; frustration ranks matching sessions; overlap reports hook-blocked, frustrated, and shared session counts. The CLI uses the SQL compiled into the installed version.
10. **Failure path:** Missing Oko index fails the named cross-source command without affecting Lake-only commands. Missing SQLite extension or denied network installation is a named DuckDB dependency failure.
11. **Cleanup or off-switch:** No Lake or Oko cleanup is required. DuckDB owns any extension cache.
12. **Next:** Inspect [Oko export](../oko/export-for-oko.md) freshness if the index does not reflect expected Lake sessions.
