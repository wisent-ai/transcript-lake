# Reset local state and uninstall

1. **Goal:** Remove the executable and, only by explicit separate decision, remove retained local Lake data.
2. **Status:** Development `0.x`; manual lifecycle contract, execution evidence pending.
3. **Risk:** Destructive and irreversible for deleted local data.
4. **Environment:** macOS terminal and known installed package/state paths.
5. **Preconditions:** Stop schedulers and writers; identify the exact `LAKE_DATA`; inspect status; decide whether to archive, retain, or delete; disconnect Oko and Parquet readers.
6. **Inputs:** Installed npm package and operator-owned Lake root.
7. **Artifacts and side effects:** Uninstall removes only the global executable. Optional manual removal deletes partitions, cursors, summaries, Parquet, and Oko export under the selected root. Vendor transcript stores are outside scope.
8. **Steps:**

```sh
LAKE="/absolute/operator-owned/lake"
transcript-lake --data-dir "$LAKE" paths
transcript-lake --data-dir "$LAKE" doctor
transcript-lake --data-dir "$LAKE" clean --target all
transcript-lake --data-dir "$LAKE" clean --target all --apply
npm uninstall --global @wisent-ai/transcript-lake
```

The CLI deliberately removes only rebuildable derived artifacts. Retain or archive authoritative Lake state by default. Removing partitions and cursors remains a separate explicit filesystem decision after uninstall.

9. **Verification:** `clean` first previews and then removes only Parquet, Oko export, and Oko staging paths. Uninstall removes the executable. Authoritative Lake state and every vendor store remain.
10. **Failure path:** An active writer blocks applied cleanup. If the state path is unexpected, stop after `paths` and do not apply cleanup or uninstall.
11. **Cleanup or off-switch:** Remove scheduler entries and environment configuration that invoke Transcript Lake. Remove backups only under the operator's retention policy.
12. **Next:** For a non-destructive restart, keep the old root and follow [rebuild into an empty root](../recovery/rebuild-into-empty-root.md).
