# Reset local state and uninstall

1. **Goal:** Remove the executable and, only by explicit separate decision, remove retained local Lake data.
2. **Risk:** Destructive and irreversible for deleted local data.
3. **Environment:** macOS terminal and known installed package/state paths.
4. **Preconditions:** Stop the supervised stream and other writers; identify the exact `LAKE_DATA`; inspect status; decide whether to archive, retain, or delete; disconnect Oko and Parquet readers.
5. **Inputs:** Installed `transcript-lake` executable and operator-owned Lake root.
6. **Artifacts and side effects:** Uninstall removes only the executable. Optional manual removal deletes partitions, cursors, stream state, Parquet, and Oko projection under the selected root. Vendor transcript stores are outside scope.
7. **Steps:**

```sh
LAKE="/absolute/operator-owned/lake"
launchctl bootout "gui/$(id -u)/com.wisent.transcript-lake-stream" 2>/dev/null || true
transcript-lake --data-dir "$LAKE" paths
transcript-lake --data-dir "$LAKE" doctor
transcript-lake --data-dir "$LAKE" clean --target all
transcript-lake --data-dir "$LAKE" clean --target all --apply
cargo uninstall transcript-lake
```

The CLI deliberately removes only rebuildable derived artifacts. Retain or archive authoritative Lake state by default. Removing partitions and cursors remains a separate explicit filesystem decision after uninstall.

8. **Observable result:** `clean` first previews and then removes only Parquet, Oko projection, and staging paths. Uninstall removes the executable. Authoritative Lake state and every vendor store remain.
9. **Failure path:** An active writer blocks applied cleanup. If the state path is unexpected, stop after `paths` and preserve it.
10. **Off-switch:** Remove `~/Library/LaunchAgents/com.wisent.transcript-lake-stream.plist` and environment configuration that invokes Transcript Lake. Retain backups according to local policy.
11. **Related operation:** For non-destructive reconstruction, keep the old root and follow [rebuild into an empty root](../recovery/rebuild-into-empty-root.md).
