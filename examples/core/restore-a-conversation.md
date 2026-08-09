# Restore one conversation

**Outcome:** read a past coding-agent conversation back in full — in order, untruncated — after the runtime that produced it has moved on, closed the session, or stopped mid-turn.

**Risk:** Read-only, external tool (DuckDB).
**Interface:** `transcript-lake sessions`, `transcript-lake search`, `transcript-lake show`.

## Preconditions

- A Lake with at least one streamed session (`transcript-lake status` reports partitions).
- DuckDB `1.5.x` on `PATH`; `show` reads through the canonical views.
- The session identifier. `sessions`, `sessions --interrupted`, and `search` all print it.

## Inputs

| Input | Meaning | Default |
|---|---|---|
| `<session-id>` | Session to reconstruct, exactly as the Lake records it | required |
| `--include <types>` | Comma-separated event types, or `all` | `user,assistant` |
| `--limit <n>` | Maximum events rendered | `2000`, maximum `50000` |
| `--json` | Machine-readable record instead of dialogue | off |

## Steps

Locate the conversation. Any of the three works; the last one is for a session that stopped without an answer:

```sh
transcript-lake search "transcript-label-trainer" --limit 5
transcript-lake sessions --runtime kimi --limit 10
transcript-lake sessions --interrupted --limit 10
```

Read it back:

```sh
transcript-lake show session_6ee965ab-4854-488a-9ca2-2515e9da3e07
```

Expected shape: an identity header (runtime, project, span, turn mix, selected types), then one block per event, oldest first, each opening with `[<timestamp>] <event_type>` and carrying the complete masked text, then the footer.

Reconstruct the complete record — reasoning, tool calls, tool results and metadata included — when the dialogue alone does not explain what happened:

```sh
transcript-lake show session_6ee965ab-4854-488a-9ca2-2515e9da3e07 --include all --limit 5000
```

Narrow to one dimension, for example only what the agent thought:

```sh
transcript-lake show session_6ee965ab-4854-488a-9ca2-2515e9da3e07 --include thinking
```

Hand the conversation to another program:

```sh
transcript-lake show session_6ee965ab-4854-488a-9ca2-2515e9da3e07 --include all --json > conversation.json
```

## Verification

- The header span matches the `first_ts`/`last_ts` that `sessions` reports for the same identifier.
- The footer reads `rendered N of M matching events`. `N == M` means the reconstruction is complete; `N < M` prints `(raise --limit for the rest)`, so a cut is visible rather than silent.
- Events appear in ascending timestamp order, which is the order the conversation happened in.

## Side effects and cleanup

None. `show` reads Lake partitions through DuckDB views and writes nothing. Redirected output is the operator's file to keep or delete.

## Representative failure

```sh
transcript-lake show not-a-session
```

fails with `unknown session "not-a-session": not present in the selected Lake (check the id or start the stream first)` and a non-zero status; an unknown `--include` value is rejected with the exact list of accepted event types. Neither case prints a partial transcript.

## Notes

Text is masked Lake evidence, not the raw vendor transcript: secrets and high-entropy strings appear as `[masked:…]` placeholders. Hostname, timestamps, project paths, and model names remain, so treat an exported conversation as sensitive.
