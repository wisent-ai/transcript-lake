# CLI tour

1. **Goal:** Use the installed `transcript-lake` executable for the complete operator workflow without importing modules or addressing source-tree files.
2. **Status:** Development `0.x`; implemented command surface, execution evidence pending.
3. **Risk:** Starts read-only, then performs local ingest, operator label writes beneath `LAKE_DATA/labels`, and optional derived mutations. Oko and DuckDB remain explicit optional dependencies.
4. **Environment:** macOS, installed Transcript Lake, operator-owned state root; DuckDB `1.5.x` for analytics/compact; Oko only for reindex/signals.
5. **Preconditions:** No writer using the selected root; enough disk; local provider sessions optional.
6. **Inputs:** One `LAKE` path, optional runtime filters, bounded limits, and optional Oko index.
7. **Artifacts and side effects:** Discovery and inspection write nothing. Ingest writes authoritative masked Lake state and export. Label writes append only beneath `LAKE_DATA/labels` and leave events, cursors, and exports untouched. Compact/export/clean affect only rebuildable derived data. Rebuild writes a different empty root.
8. **Steps:**

Discover syntax and configuration:

```sh
LAKE="/absolute/operator-owned/lake"
transcript-lake help
transcript-lake help ingest
transcript-lake --version
transcript-lake --data-dir "$LAKE" paths
transcript-lake --data-dir "$LAKE" sources --json
transcript-lake --data-dir "$LAKE" doctor
transcript-lake --data-dir "$LAKE" status --json
```

Ingest and inspect normalized evidence:

```sh
transcript-lake --data-dir "$LAKE" ingest
transcript-lake --data-dir "$LAKE" sessions --limit 20
transcript-lake --data-dir "$LAKE" events --type tool_call --limit 20
transcript-lake --data-dir "$LAKE" search "ssh" --limit 20
transcript-lake --data-dir "$LAKE" stats --days 7 --json
transcript-lake --data-dir "$LAKE" hooks --decision block
```

Annotate sessions with operator labels (writes only `LAKE_DATA/labels`; never touches the ingest lease):

```sh
transcript-lake --data-dir "$LAKE" label add <session-id> --aspect reviewed --value yes --note "human checked"
transcript-lake --data-dir "$LAKE" label list --aspect reviewed
transcript-lake --data-dir "$LAKE" label aspects --json
transcript-lake --data-dir "$LAKE" query "SELECT s.session_id, l.aspect, l.value FROM sessions s JOIN labels l USING (session_id, runtime)"
```

Re-labeling the same session and aspect appends a new record; the latest assignment wins in `label list` and `label aspects`, and the `labels` view keeps the full history.

Use advanced and optional integrations:

```sh
transcript-lake --data-dir "$LAKE" query "SELECT * FROM tokens_daily ORDER BY day DESC"
transcript-lake --data-dir "$LAKE" signals --report freshness
transcript-lake --data-dir "$LAKE" compact --source codex
transcript-lake --data-dir "$LAKE" export-oko --reindex
transcript-lake --data-dir "$LAKE" oko-refresh
```

Preview derived cleanup and perform separate-root recovery:

```sh
transcript-lake --data-dir "$LAKE" clean --target all
transcript-lake --data-dir "$LAKE" rebuild --to "/absolute/operator-owned/lake-rebuild"
```

Add `--apply` to `clean` only after downstream readers stop. `rebuild` rejects the current or non-empty target.

9. **Verification:** Read-only commands expose paths, sources, health, and state without creating a missing root. Mutation commands produce structured summaries and meaningful exit codes. Every list is bounded, filterable, and supports JSON where automation needs it.
10. **Failure path:** Invalid flags, duplicate flags, unsupported runtimes, corrupt authoritative metadata, active writers, missing optional tools, and non-empty rebuild targets fail non-zero without rewriting vendor stores.
11. **Cleanup or off-switch:** Stop scheduling mutations. Use `clean` preview/apply for derived Parquet/Oko state. Retain authoritative Lake state unless a separate explicit retention decision removes it.
12. **Next:** Choose the outcome-specific example from the [catalog](../README.md).
