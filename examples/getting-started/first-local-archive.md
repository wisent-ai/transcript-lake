# Create the first local archive

1. **Goal:** Install the development build, start the stream, and observe its masked Lake inventory.
2. **Risk:** Local mutation and provider-facing read. No network or credential use after installation.
3. **Environment:** macOS, local user account, isolated terminal, repository checkout.
4. **Preconditions:** Rust `1.85` or newer, a writable operator-owned path, no other writer using that path, and optional local agent sessions.
5. **Inputs:** One absolute `LAKE_DATA`; this example uses a temporary directory.
6. **Artifacts and side effects:** Installs one executable into `~/.cargo/bin`; writes cursors, masked partitions, live stream state, and Oko projection files beneath the selected root. Vendor stores are read-only.
7. **Steps:**

```sh
git clone https://github.com/wisent-ai/transcript-lake.git
cd transcript-lake
cargo install --path .
LAKE="$(mktemp -d)/lake"
transcript-lake
transcript-lake --data-dir "$LAKE" paths
transcript-lake --data-dir "$LAKE" sources
transcript-lake --data-dir "$LAKE" doctor
transcript-lake --data-dir "$LAKE" stream --json
```

While the stream remains open, use another terminal:

```sh
transcript-lake --data-dir "$LAKE" status
transcript-lake --data-dir "$LAKE" sessions --limit 10
transcript-lake --data-dir "$LAKE" stats --days 7
```

8. **Observable result:** The first invocation prints purpose and safe commands without creating the selected root. The stream prints one `start` record, source activity produces `commit` records, and status reports the live process plus durable cursors.
9. **Failure path:** A fatal startup or authoritative-state error exits non-zero. A source-local failure is named in the stream log and leaves that source cursor unchanged. Preserve the root and use [representative failures](../failures/representative-failures.md).
10. **Off-switch:** Stop the foreground process with SIGINT or SIGTERM. The executable and Lake data remain independent.
11. **Related operations:** Continue with the [live stream](../core/live-stream.md) or [query sessions](../core/query-sessions.md).
