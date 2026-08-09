# Upgrade and roll back an exact release

1. **Goal:** Replace one immutable Transcript Lake release with another while retaining a reversible state boundary.
2. **Status:** Release process defined; no immutable release is currently published, so commands are templates pending a real release.
3. **Risk:** Network installation and destructive/recovery state switch. Never run against the only unbacked-up Lake.
4. **Environment:** Supported macOS host, published GitHub release assets, operator-controlled backup location.
5. **Preconditions:** Exact target and prior versions, verified archive checksums and provenance, reviewed release notes and state compatibility, stopped stream, successful backup.
6. **Inputs:** Replace `<target-version>`, `<prior-version>`, archive paths, and checksum values only with values from one immutable GitHub release.
7. **Artifacts and side effects:** The extracted binary replaces the installed executable. Backup copies local Lake state. No provider transcript is modified.
8. **Steps:**

```sh
shasum -a 256 -c transcript-lake-<target-version>-<triple>.tar.gz.sha256
cp -a "$LAKE_DATA" "$LAKE_DATA.backup-<prior-version>"
tar -xzf transcript-lake-<target-version>-<triple>.tar.gz
install -m 755 transcript-lake-<target-version>-<triple>/transcript-lake "$HOME/.local/bin/transcript-lake"
transcript-lake --version
transcript-lake status
```

If rollback is required and the release notes declare state compatibility:

```sh
tar -xzf transcript-lake-<prior-version>-<triple>.tar.gz
install -m 755 transcript-lake-<prior-version>-<triple>/transcript-lake "$HOME/.local/bin/transcript-lake"
transcript-lake --version
```

Restore the backup only with the stream stopped and only when the release notes require state rollback.

9. **Verification:** Installed `--version` exactly matches the selected immutable release. Status can read existing state without mutation. Archive digest matches the published checksum and provenance names the same tag and source commit.
10. **Failure path:** Checksum mismatch, missing provenance, version mismatch, unreadable status, or incompatible state is a hard stop. Keep the prior artifact and backup; do not use `latest` to bypass identity.
11. **Cleanup or off-switch:** Retain the prior artifact and backup through the rollback window. Remove them only after the retention decision. Restart the supervised stream last.
12. **Next:** Follow the exact release's qualification and migration notes in [release policy](../../docs/RELEASES.md).
