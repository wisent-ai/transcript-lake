# Import Tama hook decisions

1. **Goal:** Ingest closed, validated adaptive-hook decision segments without double counting legacy telemetry.
2. **Status:** Development `0.x`; implemented integration, execution evidence pending.
3. **Risk:** Local mutation and provider-facing read. No hook control or runtime reload occurs.
4. **Environment:** macOS, Transcript Lake, Tama adaptive-hook producer using protocol `hooks-telemetry-segment-v1`.
5. **Preconditions:** Tama owns a ready directory containing only immutable closed segments; no manual edits; operator has read access; Lake has one writer.
6. **Inputs:** Default Tama ready path or exact `HOOKS_ADAPTIVE_SEGMENTS_READY` directory; operator-selected `LAKE_DATA`.
7. **Artifacts and side effects:** Validates checksums and framing, claims segments, writes canonical hook events, commits segment cursor metadata, then publishes acknowledgements beside producer state. When the ready directory exists, legacy mutable logs are not scanned.
8. **Steps:**

```sh
LAKE="/absolute/operator-owned/lake"
export HOOKS_ADAPTIVE_SEGMENTS_READY="/absolute/tama/telemetry-segments/ready"
transcript-lake --data-dir "$LAKE" sources
transcript-lake --data-dir "$LAKE" ingest --source hooks
transcript-lake --data-dir "$LAKE" hooks --limit 20
transcript-lake --data-dir "$LAKE" hooks --decision block --json
```

9. **Verification:** The hook runtime summary reports committed files/events or idempotent skips. A rerun skips already committed segments and can republish a missing acknowledgement. Hook events retain decision, tool, code, latency, timeout, infrastructure, segment ID, and sequence metadata after masking.
10. **Failure path:** Invalid framing, digest, ordering, filename, symlink, producer identity, or conflicting output remains unacknowledged, increments failures, makes ingest partial, and exits non-zero. Preserve the segment for Tama-side diagnosis; do not edit or rename it into acceptance.
11. **Cleanup or off-switch:** Stop Tama segment production or unset the alternate ready path. Coordinate acknowledgement and retention with Tama before removing producer files. Do not delete committed Lake events to clean producer state.
12. **Next:** Use `transcript-lake hooks --decision block` for Lake-only evidence or [cross-source signals](../duckdb/cross-source-signals.md) for Oko correlation.
