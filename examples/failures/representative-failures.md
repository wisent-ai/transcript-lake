# Diagnose representative failures

1. **Goal:** Interpret invalid input, missing dependencies, source-local failures, and writer contention without guessing from exit status alone.
2. **Risk:** Read-only except for the ordinary live stream. Never alter vendor files to manufacture a failure.
3. **Environment:** macOS terminal and installed Transcript Lake.
4. **Inputs:** The command output, stream log, and `status --json`.

## Invalid runtime

```sh
TARGET="$(mktemp -d)/must-remain-absent"
transcript-lake rebuild --to "$TARGET" --source unsupported-runtime
```

The command names the supported identifiers, exits non-zero, and leaves the
target absent.

## Missing DuckDB

```sh
PATH="/usr/bin:/bin" /absolute/path/to/transcript-lake sessions
```

The command names DuckDB as the missing optional dependency. Streaming and
status remain available.

## Incomplete SQL override

```sh
TRANSCRIPT_LAKE_SQL="$(mktemp -d)" transcript-lake sessions
```

An explicitly selected but incomplete override is rejected instead of silently
falling back to compiled views. Unset the variable to use the compiled views.

## Source-local failure

The live stream names the failed path and reason in a `failure` record, leaves
that path's cursor unchanged, and continues processing other notifications.
Truncation and same-size replacement direct recovery to a separate empty root;
permissions and storage errors can be corrected in place before restart.

## Writer contention

A compaction, export recovery, rebuild, or second stream commit that reaches the
same root while its writer lease is held reports the live owner and performs no
mutation. Never create, edit, or delete lock files manually.

## Health classification

```sh
transcript-lake doctor --json
transcript-lake status --json
```

Missing optional dependencies are warnings. Corrupt cursors or unreadable
stream state are non-zero errors without automatic repair. Diagnostics may name
paths and process identity but never unmasked transcript payloads.

For confirmed cursor damage or non-append source changes, preserve the current
root and use [rebuild into an empty root](../recovery/rebuild-into-empty-root.md).
