# Query cross-source signals with Oko

1. **Goal:** Join masked Lake events with the local Oko transcript index to inspect hook decisions and frustration signals.
2. **Risk:** Read-only, external DuckDB process. A fresh DuckDB installation may fetch and cache the SQLite extension.
3. **Environment:** macOS, DuckDB CLI `1.5.x`, valid Lake, and compatible local Oko index.
4. **Preconditions:** Oko's index is not being migrated.
5. **Inputs:** Selected Lake, Oko's read-only SQLite index, and one named report: `frustration`, `overlap`, `daily`, or `freshness`.
6. **Artifacts and side effects:** The CLI loads version-matched SQL, reads Lake, and attaches Oko read-only. DuckDB may cache its SQLite extension outside the Lake; no Oko row is modified.
7. **Steps:**

```sh
LAKE="/absolute/operator-owned/lake"
transcript-lake --data-dir "$LAKE" signals --report freshness
transcript-lake --data-dir "$LAKE" signals --report frustration --limit 20
transcript-lake --data-dir "$LAKE" signals --report overlap --json
```

8. **Observable result:** Freshness compares the Oko index with each Lake runtime; frustration ranks matching sessions; overlap reports hook-blocked, frustrated, and shared session counts.
9. **Failure path:** Missing Oko index fails the named report without affecting Lake-only commands. Missing SQLite extension is a named DuckDB dependency failure.
10. **Cleanup:** No Lake or Oko cleanup is required. DuckDB owns its extension cache.
11. **Related operation:** Inspect [Oko projection recovery](../oko/rebuild-oko.md) if the index does not reflect expected Lake sessions.
