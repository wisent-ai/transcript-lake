# Create the first local archive

1. **Goal:** Install the development build, ingest supported local transcripts, and observe the masked Lake inventory.
2. **Status:** Development `0.x`; implemented capability, execution evidence pending.
3. **Risk:** Local mutation and provider-facing read. No network or credential use after installation.
4. **Environment:** macOS, local user account, isolated terminal, repository checkout only for the development channel.
5. **Preconditions:** A Rust toolchain at version `1.85` or newer; writable operator-owned path; no scheduler using that path; local agent sessions optional.
6. **Inputs:** `LAKE_DATA` is an absolute local directory selected by the operator. This example uses a temporary directory printed by `mktemp`.
7. **Artifacts and side effects:** Installs one executable into `~/.cargo/bin`; writes cursors, masked partitions when sources exist, last-ingest summary, and Oko export beneath the temporary root. Reads but never changes vendor stores.
8. **Steps:**

```sh
git clone https://github.com/wisent-ai/transcript-lake.git
cd transcript-lake
cargo install --path .
LAKE="$(mktemp -d)/lake"
transcript-lake
transcript-lake --data-dir "$LAKE" paths
transcript-lake --data-dir "$LAKE" sources
transcript-lake --data-dir "$LAKE" doctor
transcript-lake --data-dir "$LAKE" ingest
transcript-lake --data-dir "$LAKE" sessions --limit 10
transcript-lake --data-dir "$LAKE" stats --days 7
```

9. **Verification:** The first invocation prints purpose and safe next commands without creating the selected root. Paths, sources, and doctor expose configuration and availability without mutation. Ingest prints one JSON object with `partial: false` and `failures: 0` when every discovered source succeeded. Sessions and stats expose normalized evidence only when supported records existed.
10. **Failure path:** If `partial` is true or exit is non-zero, retain the root and read stderr plus per-runtime failure counts. Use [representative failures](../failures/representative-failures.md); do not delete evidence or rerun full mode into this root.
11. **Cleanup or off-switch:** Stop future invocations, retain the printed temporary parent for inspection, then remove it only after deciding the evidence is unnecessary. `cargo uninstall transcript-lake` removes the executable but not Lake data.
12. **Next:** Continue with [incremental ingest](../core/incremental-ingest.md) or [query sessions](../core/query-sessions.md).
