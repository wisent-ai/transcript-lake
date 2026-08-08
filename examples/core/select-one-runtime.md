# Ingest one runtime

1. **Goal:** Restrict one ingest run to a supported runtime while leaving every other runtime untouched.
2. **Status:** Development `0.x`; implemented capability, execution evidence pending.
3. **Risk:** Local mutation and provider-facing read.
4. **Environment:** macOS terminal; installed Transcript Lake; isolated or established Lake.
5. **Preconditions:** A local store for the chosen runtime if a non-empty result is expected. Supported identifiers are `claude`, `codex`, `omp`, `droid`, `kimi`, and `hooks`.
6. **Inputs:** One exact runtime identifier and operator-selected `LAKE_DATA`.
7. **Artifacts and side effects:** Reads only that runtime's local roots; appends its masked events and advances only its source cursors; refreshes derived Oko export.
8. **Steps:**

```sh
LAKE="/absolute/operator-owned/lake"
transcript-lake --data-dir "$LAKE" sources
transcript-lake --data-dir "$LAKE" ingest --source codex
transcript-lake --data-dir "$LAKE" sessions --runtime codex --limit 20
```

9. **Verification:** The structured ingest summary contains only the `codex` entry under per-runtime results. No other runtime cursor advances. A valid empty Codex store is a successful zero-event result, not a fabricated session.
10. **Failure path:** `transcript-lake ingest --source gemini` must identify the unknown source, list supported values, exit non-zero, and avoid state mutation. A supported runtime parser failure yields a partial non-zero run with the runtime failure count.
11. **Cleanup or off-switch:** No cleanup is required. Choose another runtime on a later invocation; there is no persistent source-selection setting.
12. **Next:** Repeat with another supported identifier or run unfiltered [incremental ingest](incremental-ingest.md).
