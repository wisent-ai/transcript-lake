# Onboarding

This guide moves a new operator from a clean macOS environment to the first observable Transcript Lake result. Core streaming is local, requires no account or credential, and never modifies vendor transcript stores.

## Choose the channel

- **Development:** source checkout from `main`; mutable and unsupported for production.
- **Preview or stable:** an exact `v<version>` GitHub release archive, checksum, and provenance record. No preview or stable release currently exists.

Do not treat a `main` archive as an immutable release.

## Prerequisites

### Required for every installation

- macOS.
- A Rust toolchain (`cargo` and `rustc`) at version `1.85` or newer, to build the CLI. Running the built binary needs nothing else.
- Local read permission for at least one supported transcript store if you expect non-empty streaming.
- Writable local storage for `LAKE_DATA`.

Check:

```sh
uname -s
cargo --version
printf '%s\n' "${LAKE_DATA:-$HOME/.transcript-lake}"
```

Expected: `Darwin`, a cargo version of `1.85` or newer, and the intended local state path.

### Required only for SQL and compaction

- DuckDB CLI `1.5.x` available as `duckdb`.

```sh
duckdb --version
```

A missing DuckDB binary does not prevent streaming, status, masking, or Oko projection.

### Required only for Oko integration

- A compatible `oko-cli` on `PATH`, or `OKO_CLI` set to its executable path.

```sh
command -v "${OKO_CLI:-oko-cli}"
```

Oko is optional. Its absence must not prevent core streaming.

## Install an immutable release

This is the normal-user installation path after a preview or stable release exists. Select an exact version from [GitHub Releases](https://github.com/wisent-ai/transcript-lake/releases); never substitute `latest` for `VERSION`.

```sh
VERSION='<exact-version>'
ASSET='<exact-asset-name>'
BASE="https://github.com/wisent-ai/transcript-lake/releases/download/v$VERSION"
curl --fail --location --remote-name "$BASE/$ASSET"
curl --fail --location --remote-name "$BASE/$ASSET.sha256"
shasum --algorithm 256 --check "$ASSET.sha256"
tar -xzf "$ASSET"
mkdir -p "$HOME/.local/bin"
install -m 755 "${ASSET%.tar.gz}/transcript-lake" "$HOME/.local/bin/transcript-lake"
transcript-lake --version
```

`ASSET` is the archive on the release page whose architecture matches `uname -m`; the release publishes one binary archive per supported macOS architecture.

Expected: checksum verification reports `OK`; the final command prints exactly `VERSION`. Installation does not create `LAKE_DATA`, contact coding-agent services, or request elevated privileges unless the selected install directory itself requires them.

No immutable release currently exists, so these commands intentionally require an operator-selected real release version rather than inventing one.

## Development installation

Maintainers may install from the public source tree:

```sh
git clone https://github.com/wisent-ai/transcript-lake.git
cd transcript-lake
cargo install --path .
transcript-lake --version
```

`cargo install` places the binary in `~/.cargo/bin`, which must be on `PATH`. To build without installing, run `cargo build --release` and invoke `target/release/transcript-lake` directly.

This selects mutable development source and is not a production release coordinate.

## Zero state

Run without arguments before choosing a data path:

```sh
transcript-lake
```

Expected: purpose, safe starting commands, global flags, default state path, and a help URL. This command must not create files, read transcript stores, contact external services, or start background work.

## Minimum configuration

Transcript Lake defaults to `~/.transcript-lake`. Select another root for one invocation without editing shell configuration:

```sh
transcript-lake --data-dir "$HOME/.transcript-lake" paths
```

Automation may instead set `LAKE_DATA`. Global `--data-dir` wins for that process. Use an operator-owned local path; Transcript Lake does not edit shell profiles.

There are no core credentials. `OKO_CLI` is optional and needed only when asking Transcript Lake to invoke Oko.

## First successful workflow

### Starting state

- Transcript Lake is installed.
- `LAKE_DATA` identifies a directory the operator owns.
- No Transcript Lake stream or other writer is using that directory.
- Zero or more supported coding-agent stores may exist under the current user's home directory.

### Inspect without mutation

```sh
transcript-lake paths
transcript-lake sources
transcript-lake doctor
transcript-lake status
```

Expected on a clean setup: resolved paths, zero or more discovered runtime stores, optional-dependency warnings rather than core failure, an absent healthy zero-state, no partitions, no cursors, and no live stream. The Oko freshness line may report that no index or projection exists.

All four commands are read-only. `doctor` exits non-zero only for corrupt authoritative metadata or a broken installed adapter; missing DuckDB/Oko and absent source stores are warnings.

### Start the stream

Run in the foreground:

```sh
transcript-lake stream
```

The process recursively watches every supported source root. A source append is
read directly from its durable byte cursor, masked, committed to canonical
partitions and the affected Oko session projection, and then checkpointed.
There is no scan interval, timer, or refresh subprocess.

For an always-on development installation from the source checkout:

```sh
scripts/install-stream-service.sh
```

The script installs the release-profile binary outside `~/Documents`, removes
the obsolete timer/watch LaunchAgents, and loads one KeepAlive LaunchAgent named
`com.wisent.transcript-lake-stream`. Its combined structured log is
`~/Library/Logs/transcript-lake-stream.log`.

Side effects remain inside the selected `LAKE_DATA`: cursors, masked NDJSON
partitions, stream status, and Oko session projections. Vendor stores remain
unchanged, and the stream makes no network request.

### Observe and use the result

```sh
transcript-lake status
transcript-lake sessions --limit 20
transcript-lake events --type tool_call --limit 20
transcript-lake search "refactor" --limit 20
transcript-lake show <session-id>
transcript-lake label add <session-id> --aspect reviewed --value yes
transcript-lake label aspects
transcript-lake stats --days 7
```

Expected: status has a live stream record and cursor count. When supported events exist, partition counts, recent sessions/events, and statistics are non-empty, and text search returns the newest events containing the literal term. `show` reads one whole conversation back in chronological order, closing with a `rendered N of M` footer. `label add` validates the session against the Lake and appends one operator annotation beneath `LAKE_DATA/labels`. The analytics commands require DuckDB. Add `--json` for automation.

## Safe recovery and derived cleanup

For damaged cursors or a confirmed non-append source change, preserve the current root and rebuild elsewhere:

```sh
transcript-lake rebuild --to "$HOME/.transcript-lake-rebuild"
transcript-lake --data-dir "$HOME/.transcript-lake-rebuild" doctor
transcript-lake --data-dir "$HOME/.transcript-lake-rebuild" status
```

Preview and optionally remove only rebuildable derived data:

```sh
transcript-lake clean --target all
transcript-lake clean --target all --apply
```

`clean` never removes authoritative NDJSON, cursors, stream state, or vendor transcripts.

## Safe reset and uninstall

Stop the stream using the same root. Preserve evidence by moving rather than deleting the state:

```sh
STAMP=$(date -u '+%Y%m%dT%H%M%SZ')
mv "$LAKE_DATA" "$LAKE_DATA.reset-$STAMP"
```

A later stream starts from zero. Delete the retained directory only after confirming it is no longer needed. This does not remove vendor transcripts.

Uninstall the global development or release package:

```sh
cargo uninstall transcript-lake
```

A release archive installed by hand is removed the same way it was placed: `rm "$HOME/.local/bin/transcript-lake"`.

Uninstallation leaves `LAKE_DATA` intact. Remove or retain that state as a separate operator decision.

## Common failures

| Symptom | Meaning | Corrective action |
|---|---|---|
| `transcript-lake: command not found` | The installed binary is not on `PATH` | Add the install directory (`~/.cargo/bin` after `cargo install`) to `PATH`, then rerun `transcript-lake --version` |
| `unknown command` or `unknown ... flag` | Input is outside the public CLI contract | Run `transcript-lake --help` and correct the command before retrying |
| Permission error reading a vendor path | Current user cannot read that source | Correct ownership for the same user; do not elevate the stream |
| Permission error beneath `LAKE_DATA` | State root is not writable | Select an owned local path and restart; the failed source cursor did not advance |
| `duckdb failed to start` | Optional SQL dependency is absent | Install compatible DuckDB; streaming and status remain available |
| `oko-cli is not on PATH` | Optional Oko integration is unavailable | Install Oko or set `OKO_CLI`; core Lake state remains valid |
| Empty partition inventory while streaming | No supported complete records have arrived | Confirm an agent is appending within a root reported by `sources` |
| Cursor store is unreadable | Cursor JSON was damaged | Preserve the current Lake and run `rebuild --to <empty-path>`; never edit cursors or replay into existing partitions |
| Source shrank or changed without append | The append-only source contract was violated | Preserve both source and Lake, then use `rebuild --to <empty-path>` |
| Another writer holds the state lock | A mutation already owns this root | Let that writer finish or diagnose its process; do not delete a live lock |
| Interrupted stream | Process stopped after atomic checkpoints | Restart it; each source resumes at its last committed byte cursor |
| Historical replay needs too much space | Entire selected history was rebuilt | Retain current evidence and rebuild to a larger empty root or select one runtime |

Errors name the failed dependency or path. A source-local failure is logged and leaves that cursor unchanged; fatal startup or authoritative-state failure exits non-zero for the supervisor to restart.

## Machine onboarding

Automation should:

1. install an exact archive and verify its SHA-256 digest;
2. pass an explicit absolute `--data-dir` or set `LAKE_DATA`;
3. invoke `transcript-lake doctor --json` before mutation;
4. supervise one `transcript-lake stream --json` process and parse its JSONL lifecycle records;
5. treat process exit as a fatal lifecycle event and each `failure` record as source-local;
6. serialize compaction, reconstruction, and applied cleanup against the stream writer lease;
7. use `status --json`, `sources --json`, and bounded analytics commands for machine diagnostics;
8. retain selected version, source commit, archive digest, stream log, and cleanup ownership.

Continuous freshness requires exactly one explicit supervised stream. Retention, backups, and access control for the data directory remain operator-managed.

## Further reading

- Run the [canonical examples](../examples/README.md).
- Learn the [event and storage contract](LAKE.md).
- Review [release, upgrade, and rollback](RELEASES.md).
- Configure only the optional integrations you need in [integration contracts](INTEGRATIONS.md).
