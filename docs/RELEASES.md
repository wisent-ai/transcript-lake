# Release and versioning policy

## Product and public contract

The distribution coordinate is the `transcript-lake` crate in this repository. `Cargo.toml` is the single canonical source of the product version; the build compiles it into the binary, so the installed `transcript-lake --version` command cannot disagree with the artifact it came from. No independent version literal is maintained.

Transcript Lake follows Semantic Versioning. Its public contract includes:

- the `transcript-lake` executable and advertised commands and flags;
- human output explicitly documented for operators and structured JSON output documented for automation;
- `LAKE_DATA` and `OKO_CLI` configuration semantics;
- canonical event, cursor, partition, Oko-export, and provenance formats;
- adapter trait and supported runtime identifiers;
- documented masking, idempotency, retention, compatibility, and failure behavior.

While the major version is zero, an incompatible contract change advances the minor version and resets the patch version. Additive and corrective changes advance the patch version and are distinguished in `CHANGELOG.md`. Advancing to major version one is a deliberate declaration of stability.

## Channels

| Channel | Audience | Guarantee | Coordinate | Retention and movement |
|---|---|---|---|---|
| Development | Maintainers and evaluators | No compatibility guarantee | `main` commit | Moving branch; never a production install coordinate |
| Preview | Controlled early adopters | SemVer within the preview line; qualification gaps disclosed | GitHub prerelease tag and release asset | Immutable tag and asset; retained with release history |
| Stable | Operators | Documented compatibility, migration, and rollback contract | GitHub release tag and checksummed asset | Immutable; never overwritten |

There is no automatic upgrade channel. Promotion reuses the exact qualified archive and digest; it does not rebuild different bytes for another channel.

## Artifact identity

A release publishes:

- `transcript-lake-<version>-<target-triple>.tar.gz`, one per supported macOS architecture, produced by `scripts/build-release.sh` from the tagged tree and containing the release binary alongside `LICENSE`, `README.md`, `CHANGELOG.md`, `sql/`, `docs/`, and `examples/`;
- `transcript-lake-<version>-<target-triple>.tar.gz.sha256`;
- `provenance.json` containing product name, version, full source commit, tag, build timestamp, supported platform, architecture class, archive name, and SHA-256 digest;
- release notes derived from the matching `CHANGELOG.md` section;
- the examples from the same source revision inside the package archive.

The tag, GitHub release, archive, checksum, and provenance record are immutable. A correction always receives a new version.

## Release procedure

The release owner is the Transcript Lake maintainer for `wisent-ai/transcript-lake`.

1. Select a clean source revision on `main`.
2. Review README promises, public surface, configuration, persisted formats, examples, and compatibility impact.
3. Use the shared Wisent AutoVersion rule against `released-surface.json`; do not copy the versioning rule into this repository. `scripts/surface.sh` prints the current surface for that comparison.
4. Update the canonical version in `Cargo.toml`, refresh `Cargo.lock`, and move reviewed `Unreleased` notes into that version's changelog section.
5. Complete local release qualification, including safe examples and every approved test group. Credentialed or destructive qualification remains separately controlled.
6. Create an annotated `v<version>` tag on the qualified commit.
7. From that exact tag, run `sh scripts/build-release.sh`. The script refuses a dirty tree, a mismatched tag, a non-Darwin host, and an existing output archive, then builds `--locked` against the committed `Cargo.lock` and refuses a binary whose `--version` disagrees with the manifest.
8. Verify the archive digest and inspect `provenance.json`.
9. Create a GitHub release from the immutable tag and attach the archive, checksum, and provenance files without rebuilding them.
10. Install the attached archive in a clean supported environment, confirm `transcript-lake --version`, and execute the release-qualified onboarding workflow.
11. Promote a preview artifact to stable only by publishing the same bytes and digest under the stable release record.
12. Update `released-surface.json` from the artifact actually published, never from an unqualified candidate tree.

Publication uses a maintainer or automation identity scoped to repository contents and releases. Runtime transcript access, Oko access, signing identity, and publication identity remain separate.

## Compatibility and state evolution

Canonical NDJSON and cursor layouts are durable product contracts. A release must not silently reinterpret existing data.

Every persisted-state change documents:

- source and destination schema or format;
- required free space and backup;
- whether migration is lazy, eager, resumable, or forward-only;
- behavior after interruption;
- compatibility with the prior executable;
- rollback point and restoration procedure.

No migration is currently required. Derived Parquet and Oko-export data may be deleted and rebuilt from authoritative NDJSON partitions. Vendor transcripts remain externally owned and are never a Lake rollback artifact.

## Upgrade

1. Stop the supervised stream and every other process that can mutate its `LAKE_DATA`.
2. Record `transcript-lake --version` and `transcript-lake status`.
3. Back up `LAKE_DATA`, including cursors and partitions.
4. Obtain the exact target release archive, checksum, and provenance from GitHub Releases.
5. Verify SHA-256 before installation.
6. Install the archive and confirm the reported version.
7. Apply only migrations documented for that release.
8. Start the new stream and inspect its status before restoring downstream readers.

Skipping intermediate versions is supported only when every intervening release note says its migration may be skipped.

## Rollback and recovery

1. Stop the stream and every other process that can mutate the same `LAKE_DATA` root.
2. Restore the prior immutable archive by version and verified digest.
3. If the newer release changed durable state, restore the matching pre-upgrade backup. Never let two versions mutate one state root concurrently.
4. Confirm the restored version and inspect status.
5. Resume the supervised stream only after state compatibility is established.

If only derived Parquet or Oko-projection state is damaged, keep NDJSON and cursors, remove only the affected derived directory, and rebuild it with the matching supported release. If authoritative partitions or cursors are damaged, preserve them for diagnosis and restore the backup rather than attempting ad hoc repair.

## Release notes and limitations

`CHANGELOG.md` is the required release-note source. Each release records added, changed, fixed, removed, and security behavior; configuration and migrations; compatibility; operator actions; and known limitations.

No stable or preview release currently exists. Until release qualification is approved and completed, `main` is the development channel and must not be presented as an immutable production release.
