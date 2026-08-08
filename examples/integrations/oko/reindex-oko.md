# Reindex Oko after Lake export

1. **Goal:** Make newly exported Lake sessions searchable in Oko.
2. **Status:** Development `0.x`; optional integration, execution evidence pending.
3. **Risk:** Derived mutation in Oko's local SQLite index and external process invocation.
4. **Environment:** macOS, valid Lake export, compatible installed `oko-cli`.
5. **Preconditions:** Oko is configured to include `LAKE_DATA/exports/oko`; no Oko index migration is in progress; `OKO_CLI` names an alternate executable only when deliberately selected.
6. **Inputs:** Existing canonical export and Oko's local index configuration.
7. **Artifacts and side effects:** `export-oko --reindex` first updates the derived export, then invokes `oko-cli transcripts reindex --json`. `oko-refresh` invokes `oko-cli transcripts reindex` without exporting.
8. **Steps:**

```sh
LAKE="/absolute/operator-owned/lake"
transcript-lake --data-dir "$LAKE" doctor
transcript-lake --data-dir "$LAKE" export-oko --reindex
transcript-lake --data-dir "$LAKE" oko-refresh
transcript-lake --data-dir "$LAKE" signals --report freshness
```

9. **Verification:** Export stdout remains one parseable JSON object and includes reindex `{ ran: true, status: 0 }`. Both commands exit zero only when Oko reindex succeeds. Oko search or its transcript index inventory then exposes current Lake-export sessions.
10. **Failure path:** If `oko-cli` is absent, `oko-refresh` names the dependency and exits non-zero. If explicit reindex cannot start or Oko exits non-zero, `export-oko --reindex` preserves its export summary, records reindex failure, and exits non-zero. Exported files remain available for a later retry.
11. **Cleanup or off-switch:** Stop invoking reindex. Disconnect the export in Oko, then use `clean --target oko` to preview derived cleanup. Oko owns its index cleanup.
12. **Next:** Use Oko search or [cross-source signals](../duckdb/cross-source-signals.md).
