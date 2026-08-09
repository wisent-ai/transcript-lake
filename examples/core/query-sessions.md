# Query cross-runtime sessions

1. **Goal:** Retrieve recent masked session summaries across every streamed runtime, single out conversations left unfinished, and locate events containing literal text.
2. **Risk:** Read-only for the documented query; external DuckDB process.
3. **Environment:** macOS terminal, valid Lake partitions, DuckDB CLI `1.5.x` on `PATH`.
4. **Preconditions:** At least one committed session for a non-empty result.
5. **Inputs:** Selected Lake, optional runtime/project/session/type filters, a literal search term, and a bounded limit.
6. **Artifacts and side effects:** Transcript Lake writes nothing. DuckDB reads NDJSON through the pinned `sessions` and `events` views and formats rows or JSON.
7. **Steps:**

```sh
LAKE="/absolute/operator-owned/lake"
transcript-lake --data-dir "$LAKE" sessions --limit 20
transcript-lake --data-dir "$LAKE" sessions --runtime codex --project wisent --limit 20 --json
transcript-lake --data-dir "$LAKE" sessions --interrupted --limit 20
transcript-lake --data-dir "$LAKE" search "ssh" --limit 20
transcript-lake --data-dir "$LAKE" search "100%" --runtime codex --json
transcript-lake --data-dir "$LAKE" query --json "SELECT * FROM tokens_daily ORDER BY day DESC LIMIT 20"
```

9. **Verification:** `sessions` exposes runtime-native identity, project, message/tool counts, token counters, and span. Runtime is part of identity, so equal native IDs from different providers remain separate. `sessions --interrupted` narrows the same listing to conversations whose last turn was an unanswered user message or a tool call cut off mid-run, replacing the token columns with `stopped_as` and `last_user_text`; a Lake in which every conversation ended on an agent reply yields an empty result with a zero exit. `search` returns the newest events whose masked text contains the term, treating `%` and `_` as literal characters rather than wildcards; a term absent from the Lake yields an empty result with a zero exit. `query` remains available for documented or operator-owned advanced SQL.
10. **Failure path:** Missing DuckDB names the unavailable dependency and exits non-zero. Invalid runtime or out-of-range limit fails before DuckDB starts or Lake state changes.
11. **Cleanup or off-switch:** None for this read-only query. Remove any shell redirection output only if the operator created it and no longer needs it.
12. **Next:** Run a documented view such as `tokens_daily` or use [cross-source signals](../integrations/duckdb/cross-source-signals.md).
