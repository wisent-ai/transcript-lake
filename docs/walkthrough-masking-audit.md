# Walkthrough: audit synthetic masking and derived output

This walkthrough uses the same isolated 2026-08-24 synthetic fixture as the live-append walkthrough. All apparent credentials were invented specifically to exercise the three masker classes.

## Stored evidence

The initial user event became:

```json
{"ts":"2026-08-24T09:00:00.000Z","runtime":"claude","machine":"demo-host.local","session_id":"0a1b2c3d-1111-4222-8333-444455556666","project":"/tmp/demo-app","event_type":"user","text":"Deploy fails. I set [masked:assignment:37:5be474f8] and the provider token is [masked:token:36:e3bebba1] - can you check the config?"}
```

The tool result retained only assignment markers:

```json
{"event_type":"tool_result","text":"[masked:assignment:37:5be474f8]\n[masked:assignment:54:b3bc1b09]\n"}
```

The live append became:

```json
{"ts":"2026-08-24T09:01:00.000Z","event_type":"user","text":"Also scrub this pasted blob before we archive: [masked:entropy:44:d0fa7c09]"}
```

The repeated `5be474f8` demonstrates stable equality correlation. The markers disclose class and original character count, not plaintext.

## Plaintext absence check

The retained audit searched for all three invented values under the isolated `lake/`. Counts were zero in:

```text
lake/stream-status.json
lake/exports/oko/runtime=claude/<session-hash>.jsonl
lake/cursors.json
lake/events/runtime=claude/date=2026-08-24/part-b5939b619a3d.ndjson
```

It concluded `NO PLAINTEXT SECRETS UNDER lake/`. That conclusion is scoped to these exact synthetic literals and files, not to arbitrary secret shapes.

## Derived mirror

The already-ingested partition was compacted with DuckDB 1.5.5:

```text
claude: ndjson 3352 bytes -> parquet 5122 bytes (/private/tmp/lake-demo/lake/parquet/runtime=claude/events.parquet)
```

Because compaction reads canonical NDJSON, the Parquet mirror receives masked text. NDJSON remains authoritative and the mirror can be removed with `clean --target parquet --apply` after preview.

## Operator conclusions

- Whole detected hits did not cross the durable boundary in this run.
- Oko export and Parquet were downstream of canonical masking.
- A repeated fingerprint is intentionally correlatable.
- Machine/project/session metadata and ordinary text remained and must be treated as sensitive.
- Real credentials should be rotated, not merely archived behind markers.

See the [synthetic masking example](../examples/synthetic/masking-audit.md) and [full guarantee boundary](masking-guarantees.md).