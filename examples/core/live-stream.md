# Keep a Lake current with the live stream

1. **Goal:** Follow complete source bytes as agents append them, without periodic rescans.
2. **Risk:** Local mutation and provider-facing reads.
3. **Environment:** macOS terminal with installed Transcript Lake and one existing valid Lake.
4. **Preconditions:** No other writer owns the Lake; vendor transcript changes are append-only.
5. **Inputs:** Existing `LAKE_DATA` and subsequent local agent activity.
6. **Artifacts and side effects:** Appends masked canonical events and affected Oko session rows, then atomically advances only the source cursor. Vendor files remain unchanged.
7. **Steps:**

```sh
LAKE="/absolute/operator-owned/lake"
transcript-lake --data-dir "$LAKE" doctor
transcript-lake --data-dir "$LAKE" stream --json
```

In another terminal:

```sh
transcript-lake --data-dir "$LAKE" status --json
transcript-lake --data-dir "$LAKE" events --limit 20
```

8. **Observable result:** The stream prints `start`, then one `commit` line per changed source with `files`, `failures`, and `ms`. `status` reports the live process and durable cursors. Unchanged sources cause no work.
9. **Failure path:** A source truncation, same-size rewrite, invalid segment, parser failure, or unreadable file logs a named source failure without advancing that cursor. Preserve non-append sources and use [rebuild into an empty root](../recovery/rebuild-into-empty-root.md).
10. **Off-switch:** Send SIGINT or SIGTERM, or unload the supervising service. The process records `state: stopped`; retention is independent.
11. **Related operations:** [Query sessions](query-sessions.md), [compact to Parquet](../operations/compact-to-parquet.md), or [reconstruct the Oko projection](../integrations/oko/rebuild-oko.md).
