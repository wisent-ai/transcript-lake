# Rebuild into an empty root

1. **Goal:** Recover from cursor damage or non-append source change without risking the existing Lake.
2. **Risk:** Destructive/recovery workflow and provider-facing read. No deletion is part of the recovery command.
3. **Environment:** macOS, installed Transcript Lake, sufficient disk for a second full Lake.
4. **Preconditions:** Stop the stream; preserve the failed Lake and vendor sources; select a genuinely empty replacement root; capture the original error and status.
5. **Inputs:** Old `LAKE_DATA` for read-only diagnosis, then a different empty absolute path for historical replay.
6. **Artifacts and side effects:** `rebuild` reads complete selected vendor histories and writes a separate Lake, cursors, and Oko projection. The old root remains untouched.
7. **Steps:**

```sh
OLD="/absolute/operator-owned/lake-failed"
NEW="/absolute/operator-owned/lake-rebuild"
transcript-lake --data-dir "$OLD" doctor --json
transcript-lake --data-dir "$OLD" rebuild --to "$NEW"
transcript-lake --data-dir "$NEW" doctor
transcript-lake --data-dir "$NEW" status
```

8. **Observable result:** The replacement root did not exist before the command. A complete rebuild reports `full: true`, `partial: false`, zero failures, fresh cursors, and partition inventory.
9. **Failure path:** A non-empty target is rejected before replay. Insufficient space, unreadable source, or parser failure preserves both roots; recovery uses another empty root after correction.
10. **Off-switch:** After comparing aggregate inventory and intended date/runtime coverage, point the stream service at the replacement root. Retain or remove the old root according to local policy.
11. **Related operation:** Start the [live stream](../core/live-stream.md) only after the replacement root is authoritative.
