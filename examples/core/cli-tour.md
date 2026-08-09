# CLI tour

1. **Goal:** Use the installed executable for streaming, inspection, analytics, projection recovery, and safe reconstruction.
2. **Risk:** Streaming and labels mutate local Lake state; compaction and export mutate rebuildable projections. Oko and DuckDB are optional.
3. **Environment:** macOS, installed Transcript Lake, operator-owned state root; DuckDB `1.5.x` for analytics and compaction.
4. **Preconditions:** No other writer owns the selected root; enough local disk; local runtime stores are optional.
5. **Inputs:** One `LAKE` path, bounded read filters, and an optional Oko index.
6. **Side effects:** Discovery and inspection write nothing. The stream writes masked partitions, affected Oko sessions, cursors, and live status. Labels append under `labels/`. Recovery writes a different empty root.
7. **Commands:**

```sh
LAKE="/absolute/operator-owned/lake"
transcript-lake help
transcript-lake help stream
transcript-lake --version
transcript-lake --data-dir "$LAKE" paths
transcript-lake --data-dir "$LAKE" sources --json
transcript-lake --data-dir "$LAKE" doctor
transcript-lake --data-dir "$LAKE" status --json
```

Start `transcript-lake --data-dir "$LAKE" stream --json` in one terminal. In
another terminal, inspect normalized evidence:

```sh
transcript-lake --data-dir "$LAKE" sessions --limit 20
transcript-lake --data-dir "$LAKE" events --type tool_call --limit 20
transcript-lake --data-dir "$LAKE" search "ssh" --limit 20
transcript-lake --data-dir "$LAKE" show <session-id>
transcript-lake --data-dir "$LAKE" stats --days 7 --json
transcript-lake --data-dir "$LAKE" hooks --decision block
```

Add operator labels without taking the stream writer lease:

```sh
transcript-lake --data-dir "$LAKE" label add <session-id> --aspect reviewed --value yes --note "human checked"
transcript-lake --data-dir "$LAKE" label list --aspect reviewed
transcript-lake --data-dir "$LAKE" label aspects --json
```

Run optional derived operations while no stream commit is in flight:

```sh
transcript-lake --data-dir "$LAKE" query "SELECT * FROM tokens_daily ORDER BY day DESC"
transcript-lake --data-dir "$LAKE" signals --report freshness
transcript-lake --data-dir "$LAKE" compact --source codex
transcript-lake --data-dir "$LAKE" rebuild-oko --reindex
transcript-lake --data-dir "$LAKE" clean --target all
transcript-lake --data-dir "$LAKE" rebuild --to "/absolute/operator-owned/lake-rebuild"
```

`clean` is a preview unless `--apply` is present. `rebuild` rejects the current
or any non-empty target.

8. **Observable result:** Read commands expose paths, sources, health, live state, and bounded evidence. The stream emits `start`, `commit`, source-local `failure`, and `stop` records without printing transcript payloads.
9. **Failure path:** Invalid flags, duplicate flags, unsupported runtimes, corrupt authoritative metadata, writer conflicts, missing optional tools, and non-empty rebuild targets fail without rewriting vendor stores.
10. **Off-switch:** Stop the foreground stream or unload its supervisor. Use `clean` only for rebuildable projections; authoritative Lake retention is independent.
