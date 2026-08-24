# Quick start: a synthetic local Lake

This quick start was executed against Transcript Lake `0.2.0` on 2026-08-24 with DuckDB `1.5.5`. It used a synthetic Claude-format fixture under an isolated `HOME`; no real transcript was opened and no credential was used.

## 1. Build and select an isolated root

```sh
cargo build
export HOME=/private/tmp/lake-demo/home
export LAKE_DATA=/private/tmp/lake-demo/lake
```

Create a synthetic `~/.claude/projects/-tmp-demo-app/<session>.jsonl` fixture containing user, assistant, thinking, tool-call, and tool-result records. Use invented values only. The executed fixture used session id `0a1b2c3d-1111-4222-8333-444455556666` and project `/tmp/demo-app`.

## 2. Start the foreground stream

```sh
transcript-lake stream
```

The retained stream log showed catch-up followed by a real filesystem append:

```text
2026-08-24T23:11:40.372Z stream start roots=1
2026-08-24T23:11:40.432Z stream catch-up files=1 streamed=1 failures=0 ms=38
2026-08-24T23:11:47.780Z stream event paths=1 first=/private/tmp/lake-demo/home/.claude/projects/-tmp-demo-app/0a1b2c3d-1111-4222-8333-444455556666.jsonl
2026-08-24T23:11:47.853Z stream commit files=1 failures=0 ms=50
```

Run the stream under a supervisor for continuous use. Stop it with `SIGINT` or `SIGTERM`; the process writes final stream state and releases its writer lease.

## 3. Inspect the result

In another terminal with the same `HOME` and `LAKE_DATA`:

```sh
transcript-lake sources
transcript-lake status
```

The executed source discovery was:

```text
claude: transcripts, 1 files
  /private/tmp/lake-demo/home/.claude/projects
codex: not found, 0 files
omp: not found, 0 files
droid: not found, 0 files
kimi: not found, 0 files
hooks: not found, 0 files
```

The status snapshot reported one 3,352-byte Claude partition, one healthy cursor, a running stream with zero failures, and Lake data fresher than the absent Oko index.

## 4. Verify the privacy boundary

The stored synthetic user event contained these markers:

```json
{"runtime":"claude","event_type":"user","text":"Deploy fails. I set [masked:assignment:37:5be474f8] and the provider token is [masked:token:36:e3bebba1] - can you check the config?"}
{"runtime":"claude","event_type":"user","text":"Also scrub this pasted blob before we archive: [masked:entropy:44:d0fa7c09]"}
```

A literal search for all three invented plaintext values across the isolated `lake/` tree returned zero matches in the cursor, partition, stream status, and Oko export files. This demonstrates the three documented masker classes for that fixture; it is not a claim that arbitrary text is anonymized.

## 5. Read and derive

With DuckDB `1.5.x` on `PATH`:

```sh
transcript-lake sessions --limit 20
transcript-lake show 0a1b2c3d-1111-4222-8333-444455556666 --include all
transcript-lake search "Rotate the leaked key"
transcript-lake compact --source claude
```

The executed compaction reported:

```text
claude: ndjson 3352 bytes -> parquet 5122 bytes (/private/tmp/lake-demo/lake/parquet/runtime=claude/events.parquet)
```

NDJSON remains authoritative; Parquet is rebuildable. Continue with the [live-append walkthrough](walkthrough-live-append.md), [masking audit walkthrough](walkthrough-masking-audit.md), and [CLI reference](cli-reference.md).