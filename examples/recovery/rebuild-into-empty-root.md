# Rebuild into an empty root

1. **Goal:** Recover from cursor damage or non-append source change without risking the existing Lake.
2. **Status:** Development `0.x`; implemented recovery contract, execution evidence pending.
3. **Risk:** Destructive/recovery workflow and provider-facing read. No deletion is part of the recovery command.
4. **Environment:** macOS, installed Transcript Lake, sufficient disk for a second full Lake.
5. **Preconditions:** Stop schedulers; preserve the failed Lake and vendor sources; select a genuinely empty replacement root; capture the original error and status.
6. **Inputs:** Old `LAKE_DATA` for read-only diagnosis, then a different empty absolute path for full ingest.
7. **Artifacts and side effects:** Full ingest reads complete supported vendor histories and writes a separate Lake, cursors, summary, and derived export. The old root remains untouched.
8. **Steps:**

```sh
export OLD_LAKE_DATA="/absolute/operator-owned/lake-failed"
LAKE_DATA="$OLD_LAKE_DATA" transcript-lake status
export LAKE_DATA="/absolute/operator-owned/lake-rebuild"
transcript-lake ingest --full
transcript-lake status
```

9. **Verification:** The replacement root did not exist before the command. Full ingest emits `full: true`, `partial: false`, and zero failures for a complete rebuild. Status shows fresh cursors and partition inventory. Compare only aggregate inventories and intended date/runtime coverage; do not expose raw transcript text in evidence.
10. **Failure path:** If the replacement path is non-empty, abort before ingest. Insufficient space, unreadable source, or parser failure exits non-zero; preserve both roots and retry only into another empty root after correction. Never delete the old root to make a failed rebuild appear successful.
11. **Cleanup or off-switch:** After bounded verification and an explicit retention decision, switch schedulers to the replacement root. Archive or securely delete the old root according to policy. Deletion is manual and irreversible.
12. **Next:** Resume [incremental ingest](../core/incremental-ingest.md) only after the new root is authoritative.
