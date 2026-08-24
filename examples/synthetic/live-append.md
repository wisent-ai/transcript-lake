# Synthetic example: live append

**Status:** executed 2026-08-24. **Risk:** isolated local mutation only. **Input:** invented Claude-format JSONL. **No real transcripts, credentials, provider calls, or network actions.**

1. Set an isolated `HOME` and `LAKE_DATA` under a directory you created.
2. Create `HOME/.claude/projects/-tmp-demo-app/0a1b2c3d-1111-4222-8333-444455556666.jsonl` with invented complete JSONL records matching Claude's documented adapter format.
3. Start `transcript-lake stream` in the foreground.
4. Wait for `stream catch-up ... failures=0`.
5. Append one complete invented user record and newline.
6. Observe `stream event paths=1` and `stream commit files=1 failures=0`.
7. Run `transcript-lake status` from another terminal.
8. Stop the stream with `SIGINT` and retain or delete only the isolated directory you created.

The executed run emitted:

```text
stream catch-up files=1 streamed=1 failures=0 ms=38
stream event paths=1 first=/private/tmp/lake-demo/home/.claude/projects/-tmp-demo-app/0a1b2c3d-1111-4222-8333-444455556666.jsonl
stream commit files=1 failures=0 ms=50
```

Observable success is a healthy cursor, one Claude partition, an Oko projection, and zero failures. A zero-file commit means no complete owned append was ingested; inspect path identity and newline completion rather than assuming success.

Do not adapt this example by pointing it at the user's actual home for documentation or qualification. The detailed retained output is in [the walkthrough](../../docs/walkthrough-live-append.md).