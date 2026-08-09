# Build release assets

1. **Goal:** Produce an attributable binary archive, checksum, and provenance record for one exact release candidate.
2. **Status:** Release tooling implemented; publication and qualification pending.
3. **Risk:** Local build mutation and publication preparation. Building does not publish.
4. **Environment:** Clean exact source tag, Rust toolchain at version `1.85` or newer, macOS host, maintainer-controlled workstation.
5. **Preconditions:** Approved SemVer change, updated changelog and released-surface baseline, exact tag, clean source, completed required qualification, no secrets in source or environment-derived artifacts.
6. **Inputs:** Repository source, `Cargo.toml` metadata, committed `Cargo.lock`, tag, current source revision, generated public surface.
7. **Artifacts and side effects:** Writes `dist/` with the binary tarball, SHA-256 checksum, and provenance JSON, and writes `target/` with the release build. Does not modify Lake data or provider stores.
8. **Steps:**

```sh
cargo run --release -- --version
sh scripts/surface.sh > released-surface.json
sh scripts/build-release.sh
```

Publication is deliberately absent. It requires a separate maintainer-approved GitHub release workflow tied to the same tag and source revision.

9. **Verification:** Provenance names product, version, source revision, tag, archive filename, SHA-256, platform, and architecture. The archived binary's `--version` matches the manifest version and the tag; `scripts/build-release.sh` refuses the build otherwise. The checksum validates the exact tarball.
10. **Failure path:** Missing or dirty tag, version mismatch, surface mismatch, package failure, secret finding, or unqualified capability blocks publication. Delete incomplete `dist/`, correct the source, and rebuild from a new clean checkout; never rewrite an immutable release.
11. **Cleanup or off-switch:** `dist/` is ignored derived output. Retain approved assets with the release or remove rejected local candidates. Never call an untagged build a release.
12. **Next:** Follow [release policy](../../docs/RELEASES.md) for publication, channels, rollback, and retirement.
