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
transcript-lake status
npm uninstall --global @wisent-ai/transcript-lake
```

Retain or archive `LAKE_DATA` by default. Only after an explicit deletion decision:

```sh
rm -rf -- "$LAKE_DATA"
```

9. **Verification:** `command -v transcript-lake` finds no executable after uninstall. Retained Lake paths still exist unless the separate removal command was deliberately run. No file under Claude, Codex, OMP, Droid, Kimi, Oko, or Tama vendor roots is removed.
10. **Failure path:** If the state path is empty, relative, unexpected, shared, or still in use, do not run removal. Reinstalling the executable does not recover deleted data; restore only from an operator-owned archive.
11. **Cleanup or off-switch:** Remove scheduler entries and environment configuration that invoke Transcript Lake. Remove backups only under the operator's retention policy.
12. **Next:** For a non-destructive restart, keep the old root and follow [rebuild into an empty root](../recovery/rebuild-into-empty-root.md).
