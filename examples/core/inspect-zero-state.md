# Inspect zero state and product identity

1. **Goal:** Confirm the installed product identity and inspect configuration without creating Lake state.
2. **Status:** Development `0.x`; implemented capability, execution evidence pending.
3. **Risk:** Read-only.
4. **Environment:** macOS terminal with installed `transcript-lake`.
5. **Preconditions:** Choose a path that does not exist and is not used by another process.
6. **Inputs:** An absolute `LAKE_DATA` path beneath an operator-owned temporary parent.
7. **Artifacts and side effects:** None. These commands must not create the selected root or contact external tools.
8. **Steps:**

```sh
PARENT="$(mktemp -d)"
LAKE="$PARENT/not-created"
transcript-lake
transcript-lake --help
transcript-lake help doctor
transcript-lake --version
transcript-lake --data-dir "$LAKE" paths
transcript-lake --data-dir "$LAKE" sources
transcript-lake --data-dir "$LAKE" doctor --json
transcript-lake --data-dir "$LAKE" status --json
```

9. **Verification:** Help states the product purpose, safe starting commands, supported operations, state default, and help URL. Version is a Semantic Versioning value matching the installed binary. Status names the selected path and reports no partitions, cursors, or last ingest. The `not-created` directory remains absent.
10. **Failure path:** An unknown command must print `error: unknown command`, guidance, and exit non-zero without creating state. The version is compiled into the binary, so a version that does not match the artifact you installed means you are running a different `transcript-lake` on `PATH`; resolve which one before continuing.
11. **Cleanup or off-switch:** Remove only the empty temporary parent created by this example.
12. **Next:** Create the [first local archive](../getting-started/first-local-archive.md).
