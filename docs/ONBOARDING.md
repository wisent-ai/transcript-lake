# Onboarding

This guide moves a new operator from a clean macOS environment to the first observable Transcript Lake result. Core ingestion is local, requires no account or credential, and never modifies vendor transcript stores.

## Choose the channel

- **Development:** source checkout from `main`; mutable and unsupported for production.
- **Preview or stable:** an exact `v<version>` GitHub release archive, checksum, and provenance record. No preview or stable release currently exists.

Do not treat a `main` archive as an immutable release.

## Prerequisites

### Required for every installation

- macOS.
- Node.js version twenty or newer.
- Local read permission for at least one supported transcript store if you expect non-empty ingestion.
- Writable local storage for `LAKE_DATA`.

Check:

```sh
uname -s
node --version
printf '%s\n' "${LAKE_DATA:-$HOME/.transcript-lake}"
```

Expected: `Darwin`, a Node version beginning with `v20` or newer, and the intended local state path.

### Required only for SQL and compaction

- DuckDB CLI `1.5.x` available as `duckdb`.

```sh
duckdb --version
```

A missing DuckDB binary does not prevent ingest, status, masking, or Oko export.

### Required only for Oko integration

- A compatible `oko-cli` on `PATH`, or `OKO_CLI` set to its executable path.

```sh
command -v "${OKO_CLI:-oko-cli}"
```

Oko is optional. Its absence must not prevent core ingestion.

## Install an immutable release

This is the normal-user installation path after a preview or stable release exists. Select an exact version from [GitHub Releases](https://github.com/wisent-ai/transcript-lake/releases); never substitute `latest` for `VERSION`.

```sh
VERSION='<exact-version>'
BASE="https://github.com/wisent-ai/transcript-lake/releases/download/v$VERSION"
curl --fail --location --remote-name "$BASE/transcript-lake-$VERSION.tgz"
curl --fail --location --remote-name "$BASE/transcript-lake-$VERSION.tgz.sha256"
shasum --algorithm 256 --check "transcript-lake-$VERSION.tgz.sha256"
npm install --global "./transcript-lake-$VERSION.tgz"
transcript-lake --version
```

Expected: checksum verification reports `OK`; the final command prints exactly `VERSION`. Installation does not create `LAKE_DATA`, contact coding-agent services, or request elevated privileges unless the operator's npm prefix itself requires them.

No immutable release currently exists, so these commands intentionally require an operator-selected real release version rather than inventing one.

## Development installation

Maintainers may install from the public source tree:

```sh
git clone https://github.com/wisent-ai/transcript-lake.git
cd transcript-lake
npm install --global .
transcript-lake --version
```

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
- No Transcript Lake process or scheduler is using that directory.
- Zero or more supported coding-agent stores may exist under the current user's home directory.

### Inspect without mutation

```sh
transcript-lake paths
transcript-lake sources
transcript-lake doctor
transcript-lake status
```

Expected on a clean setup: resolved paths, zero or more discovered runtime stores, optional-dependency warnings rather than core failure, an absent healthy zero-state, no partitions, no cursors, and no last ingest. The Oko freshness line may report that no index or export exists.

All four commands are read-only. `doctor` exits non-zero only for corrupt authoritative metadata or a broken installed adapter; missing DuckDB/Oko and absent source stores are warnings.

### Ingest

```sh
transcript-lake ingest
```

Expected: one JSON object containing `finishedAt`, `source`, `full`, `perRuntime`, `maskCounts`, `durationMs`, and `okoExport`. Supported stores contribute counts; absent stores are skipped. Success with no source stores creates an empty, valid run summary rather than fake events.

Side effects are limited to the selected `LAKE_DATA`: cursors, masked NDJSON partitions when records exist, the last-ingest summary, and the derived Oko export. Vendor stores remain unchanged. The command makes no network request.

### Observe and use the result

```sh
transcript-lake status
transcript-lake sessions --limit 20
transcript-lake events --type tool_call --limit 20
transcript-lake stats --days 7
```

Expected: status has a last-ingest timestamp and cursor count. When supported events existed, partition counts, recent sessions/events, and statistics are non-empty. The analytics commands require DuckDB. Add `--json` for automation.

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

`clean` never removes authoritative NDJSON, cursors, last-ingest evidence, or vendor transcripts.

## Safe reset and uninstall

Stop every scheduler using the same root. Preserve evidence by moving rather than deleting the state:

```sh
STAMP=$(date -u '+%Y%m%dT%H%M%SZ')
mv "$LAKE_DATA" "$LAKE_DATA.reset-$STAMP"
```

A later ingest starts from zero. Delete the retained directory only after confirming it is no longer needed. This does not remove vendor transcripts.

Uninstall the global development or release package:

```sh
npm uninstall --global @wisent-ai/transcript-lake
```

Uninstallation leaves `LAKE_DATA` intact. Remove or retain that state as a separate operator decision.

## Common failures

| Symptom | Meaning | Corrective action |
|---|---|---|
| `node: command not found` | Required runtime is absent | Install supported Node.js, then rerun `transcript-lake --version` |
| `unknown command` or `unknown ... flag` | Input is outside the public CLI contract | Run `transcript-lake --help` and correct the command before retrying |
| Permission error reading a vendor path | Current user cannot read that source | Correct ownership or omit that runtime with `--source`; do not elevate the whole ingest unnecessarily |
| Permission error beneath `LAKE_DATA` | State root is not writable | Select an owned local path and retry; no partial cursor should be trusted until status succeeds |
| `duckdb failed to start` | Optional SQL dependency is absent | Install compatible DuckDB; ingest and status remain available |
| `oko-cli is not on PATH` | Optional Oko integration is unavailable | Install Oko or set `OKO_CLI`; core Lake state remains valid |
| Empty partition inventory after ingest | No supported records were discovered | Confirm the agent has local sessions and that its path matches the supported adapter contract |
| Cursor store is unreadable | Cursor JSON was damaged | Preserve the current Lake and run `rebuild --to <empty-path>`; never edit cursors or replay into existing partitions |
| Source shrank or changed without append | The append-only source contract was violated | Preserve both source and Lake, then use `rebuild --to <empty-path>` |
| Another writer holds the state lock | A mutation already owns this root | Let that writer finish or diagnose its process; do not delete a live lock |
| Interrupted ingest | Process stopped after atomic checkpoints | Rerun the same incremental command; flushed batches remain checkpointed |
| Full replay needs too much space | Entire local history was selected | Retain current evidence and rebuild to a larger empty root or select one runtime |

Errors should name the failed dependency or path and exit non-zero. Retrying incremental ingest is safe; retrying a full ingest may repeat substantial local work but must not alter vendor stores.

## Machine onboarding

Automation should:

1. install an exact archive and verify its SHA-256 digest;
2. pass an explicit absolute `--data-dir` or set `LAKE_DATA`;
3. invoke `transcript-lake doctor --json` before mutation;
4. invoke `transcript-lake ingest` and parse JSON stdout only after a zero exit;
5. treat stderr and non-zero exit as failure rather than partial success;
6. serialize mutations per state root;
7. use `status --json`, `sources --json`, and bounded analytics commands for machine diagnostics;
8. retain selected version, source commit, archive digest, ingest summary, and cleanup ownership.

No hidden service is required. Scheduling, retention, backups, and access control for the data directory remain operator-managed.

## Next steps

- Run the [canonical examples](../examples/README.md).
- Learn the [event and storage contract](LAKE.md).
- Review [release, upgrade, and rollback](RELEASES.md).
- Configure only the optional integrations you need in [integration contracts](INTEGRATIONS.md).
