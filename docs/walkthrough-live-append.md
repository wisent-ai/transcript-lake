# Walkthrough: catch up and commit a synthetic live append

This walkthrough records an execution performed on 2026-08-24 against an isolated synthetic Claude transcript. It demonstrates the online path without opening any real transcript.

## Fixture

The isolated environment used:

```text
HOME=/private/tmp/lake-demo/home
LAKE_DATA=/private/tmp/lake-demo/lake
session=0a1b2c3d-1111-4222-8333-444455556666
project=/tmp/demo-app
```

The initial JSONL contained six synthetic canonical outcomes: user, thinking, assistant, tool call, tool result, and assistant. After the stream reached running state, one more synthetic user line was appended.

## Observed lifecycle

```text
2026-08-24T23:11:40.372Z stream start roots=1
2026-08-24T23:11:40.432Z stream catch-up files=1 streamed=1 failures=0 ms=38
2026-08-24T23:11:47.780Z stream event paths=1 first=/private/tmp/lake-demo/home/.claude/projects/-tmp-demo-app/0a1b2c3d-1111-4222-8333-444455556666.jsonl
2026-08-24T23:11:47.853Z stream commit files=1 failures=0 ms=50
```

The watcher was installed before catch-up. Catch-up consumed the initial complete lines and checkpointed them. The later FSEvents notification resolved one owned path; the append produced one committed file and zero failures.

## Observed artifacts

```text
lake/cursors.json
lake/events/runtime=claude/date=2026-08-24/part-b5939b619a3d.ndjson
lake/exports/oko/runtime=claude/f6bcd0f67268e3231e23d42249abfb878fc385f456d06c0b938d30e66fdb85ef.jsonl
lake/stream-status.json
```

This shows the atomic unit spans canonical partition, Oko session projection, source cursor, and status. The partition had seven events and the latest line timestamp was `2026-08-24T09:01:00.000Z`.

`status` reported:

```text
data dir: /private/tmp/lake-demo/lake
  claude: 1 partition files, 3352 bytes
cursors: healthy, 1 tracked files, newest 2026-08-24T23:11:47.768Z
stream: running, updated 2026-08-24T23:11:47.853Z, files 1, failures 0
```

Oko's index did not exist, and freshness correctly selected `lake`. Oko absence did not block the stream or projection write.

## What this proves

For this fixture and revision, native watch notification, append-only cursor resume, partition placement, Oko projection, and operational status worked end to end. It does not prove handling for other vendors or arbitrary malformed input.

Reproduce only with invented local fixture data. See the executable [synthetic live example](../examples/synthetic/live-append.md) and [ingestion reference](ingestion-reference.md).