# Inspect zero state and product identity

1. **Goal:** Confirm the installed product identity and inspect configuration without creating Lake state.
2. **Risk:** Read-only.
3. **Environment:** macOS terminal with installed `transcript-lake`.
4. **Preconditions:** Choose a path that does not exist and is not used by another process.
5. **Inputs:** An absolute `LAKE_DATA` path beneath an operator-owned temporary parent.
6. **Artifacts and side effects:** None. These commands must not create the selected root or contact external tools.
7. **Steps:**

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

8. **Observable result:** Help states the product purpose, commands, state default, and help URL. Version identifies the installed binary. Status names the selected path and reports no partitions, cursors, or live stream; the selected root remains absent.
9. **Failure path:** An unknown command prints `error: unknown command`, guidance, and exits non-zero without creating state.
10. **Cleanup:** Remove only the empty temporary parent created by this example.
11. **Related operation:** Create the [first local archive](../getting-started/first-local-archive.md).
