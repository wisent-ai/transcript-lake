# Diagnose representative failures

1. **Goal:** Recognize fail-closed behavior for invalid input, missing dependencies, partial ingest, and writer contention.
2. **Status:** Development `0.x`; failure contracts implemented, execution evidence pending.
3. **Risk:** Read-only except isolated ingest cases, which create local state. Do not provoke failures against a shared or only production-like Lake.
4. **Environment:** macOS terminal, installed Transcript Lake, isolated temporary root for mutation scenarios.
5. **Preconditions:** Preserve stderr and structured stdout separately; know whether the scenario is expected; never alter vendor files to force a failure.
6. **Inputs:** Scenario-specific commands below.
7. **Artifacts and side effects:** Invalid input and missing dependencies should create no Lake state. Partial ingest may preserve already flushed partitions and cursors. Writer contention leaves the incumbent writer's state untouched.
8. **Steps and observable outcomes:**

**Invalid runtime, no mutation**

```sh
LAKE="$(mktemp -d)/must-remain-absent"
transcript-lake --data-dir "$LAKE" ingest --source unsupported-runtime
```

Expected: supported identifiers are named, exit is non-zero, and the selected root remains absent.

**Missing DuckDB, no mutation**

```sh
PATH="/usr/bin:/bin" /absolute/path/to/transcript-lake --data-dir "$LAKE" sessions
```

Expected: a named DuckDB dependency error and non-zero exit. Use this only when `transcript-lake` itself is addressed by an absolute path or remains on that restricted `PATH`.

**Partial ingest**

Run ordinary incremental ingest and inspect its real result; do not corrupt a source to manufacture evidence:

```sh
transcript-lake --data-dir "$LAKE" ingest
```

Expected on an organic source/parser failure: parseable summary with `partial: true`, non-zero aggregate failures, affected per-runtime failures, and non-zero exit. Previously flushed checkpoints remain.

**Writer contention**

If an ingest is already legitimately running, a second ordinary ingest against the same root should report the live lock owner and exit non-zero. Do not create or edit lock files manually.

**Health classification**

```sh
transcript-lake --data-dir "$LAKE" doctor --json
transcript-lake --data-dir "$LAKE" status --json
```

Expected: optional dependency absence is a warning; corrupt cursors or summaries are non-zero errors without automatic repair.

9. **Verification:** A failure is verified by its classified message, non-zero exit, and bounded state effect—not by matching an implementation stack trace. Secret-bearing transcript payloads never appear in diagnostics.
10. **Recovery:** Correct invalid input; install pinned DuckDB; retain and diagnose partial source failures; let a live writer finish. Remove a stale lock only through product recovery behavior, never manual deletion while ownership is uncertain.
11. **Cleanup or off-switch:** Remove only isolated temporary parents created for no-mutation checks. Preserve organic failure evidence until diagnosis. No cloud or provider cleanup exists.
12. **Next:** Use [rebuild into an empty root](../recovery/rebuild-into-empty-root.md) only for confirmed cursor damage or non-append source changes.
