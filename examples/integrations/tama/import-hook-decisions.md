# Stream Tama hook decisions

1. **Goal:** Stream closed, validated adaptive-hook decision segments without double counting legacy telemetry.
2. **Risk:** Local mutation and provider-facing read. No hook control or runtime reload occurs.
3. **Environment:** macOS, Transcript Lake, and a Tama producer using `hooks-telemetry-segment-v1`.
4. **Preconditions:** Tama owns an immutable ready directory; the stream user can read it; no other Lake writer is active.
5. **Inputs:** The default Tama ready path or exact `HOOKS_ADAPTIVE_SEGMENTS_READY`, plus `LAKE_DATA`.
6. **Artifacts and side effects:** Validates checksums and framing, claims each notified segment, writes canonical hook events, commits segment cursor metadata, then publishes an acknowledgement. When the ready directory exists, legacy mutable logs are excluded.
7. **Steps:**

```sh
LAKE="/absolute/operator-owned/lake"
export HOOKS_ADAPTIVE_SEGMENTS_READY="/absolute/tama/telemetry-segments/ready"
transcript-lake --data-dir "$LAKE" sources
transcript-lake --data-dir "$LAKE" stream --json
```

In another terminal:

```sh
transcript-lake --data-dir "$LAKE" hooks --limit 20
transcript-lake --data-dir "$LAKE" hooks --decision block --json
```

8. **Observable result:** A valid segment produces a stream `commit`, canonical hook events, and one content-matched acknowledgement. Already committed segments are skipped idempotently and a missing acknowledgement can be republished.
9. **Failure path:** Invalid framing, digest, ordering, filename, symlink, producer identity, or conflicting output remains unacknowledged. The stream logs the segment failure and continues unrelated paths without advancing that segment cursor.
10. **Off-switch:** Stop Tama segment production or restart the stream without the alternate ready path. Coordinate acknowledgement retention with Tama; committed Lake events remain ordinary evidence.
