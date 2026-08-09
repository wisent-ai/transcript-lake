# Reindex Oko after projection changes

1. **Goal:** Make Lake sessions searchable in Oko after the live projection or a reconstruction changes.
2. **Risk:** Derived mutation in Oko's local SQLite index and external process invocation.
3. **Environment:** macOS, valid Lake projection, compatible installed `oko-cli`.
4. **Preconditions:** Oko includes `LAKE_DATA/exports/oko`; no Oko index migration is in progress; `OKO_CLI` is set only for a deliberate alternate executable.
5. **Inputs:** Existing canonical projection and Oko's local index configuration.
6. **Artifacts and side effects:** `oko-refresh` invokes Oko without rebuilding Lake files. `rebuild-oko --reindex` first reconstructs every projection file, then invokes the compatible JSON reindex.
7. **Steps:**

```sh
LAKE="/absolute/operator-owned/lake"
transcript-lake --data-dir "$LAKE" doctor
transcript-lake --data-dir "$LAKE" oko-refresh
transcript-lake --data-dir "$LAKE" signals --report freshness
```

Use `rebuild-oko --reindex` only when projection reconstruction is also needed.

8. **Observable result:** The command exits zero only when Oko reindex succeeds. Oko's transcript inventory then exposes current Lake-projected sessions.
9. **Failure path:** If `oko-cli` is absent or exits non-zero, the command names the dependency or status and leaves the projection intact for later reindex.
10. **Off-switch:** Stop invoking reindex. Disconnect the projection in Oko before previewing its removal with `clean --target oko`.
