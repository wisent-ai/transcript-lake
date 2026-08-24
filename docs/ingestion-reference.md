# Ingestion pipeline reference

## Supported inputs

| Runtime | Source root and candidates | Identity/project | Emitted event types |
|---|---|---|---|
| `claude` | `~/.claude/projects/<encoded-cwd>/<id>.jsonl` | record `sessionId`; record `cwd` / encoded directory | `user`, `assistant`, `thinking`, `tool_call`, `tool_result`; usage on assistant content |
| `codex` | `~/.codex/sessions/<year>/<month>/<day>/rollout-*.jsonl` | session metadata and rollout identity | `meta`, `user`, `assistant`, `thinking`, `tool_call`, `tool_result` |
| `omp` | `~/.omp/agent/sessions/<encoded-cwd>/<stamp>_<id>.jsonl` | session header / file identity; encoded cwd | `meta`, `user`, `assistant`, `thinking`, `tool_call`, `tool_result` |
| `droid` | `~/.factory/sessions/<encoded-cwd>/<uuid>.jsonl` plus `.settings.json` sidecar | first session-start line / UUID; encoded cwd | conversation events plus sidecar `meta` |
| `kimi` | `~/.kimi-code/sessions/wd_*/session_*/agents/main/wire.jsonl`, nearby `state.json` | state/session directory | `meta`, `user`, `assistant`, `thinking`, `tool_call`, `tool_result`; configuration/system text excluded |
| `hooks` | `$HOOKS_ADAPTIVE_SEGMENTS_READY/*.jsonl`, default `~/.hooks-adaptive/telemetry-segments/ready`; legacy `~/.hooks-adaptive/telemetry{,.prev}.jsonl` | record session/project | `hook_decision` |

Directory disappearance during discovery yields no entries. Unowned paths and unrelated artifacts are ignored. Parsers tolerate malformed and torn lines by emitting no event; complete prior lines still stream.

## Raw-to-canonical transformation

An adapter returns `RawEvent { ts, session_id, project, event_type, text, tool_name, model, tokens_in, tokens_out, extra }`. The stream adds stable `runtime` and local `machine`, masks every retained string, caps values, then serializes this exact canonical order:

1. `ts: string|null`
2. `runtime: string`
3. `machine: string`
4. `session_id: string|null`
5. `project: string|null`
6. `event_type: string`
7. `text: string`
8. `tool_name: string|null`
9. `model: string|null`
10. `tokens_in: integer|null`
11. `tokens_out: integer|null`
12. `extra: object`

Allowed types are `user`, `assistant`, `thinking`, `tool_call`, `tool_result`, `meta`, and `hook_decision`. `extra.source_stem_hash` is SHA-256 of the source filename stem; the plaintext stem is not stored.

## Catch-up and live append

1. Create the platform watcher (`FSEvents` on macOS, `inotify` on Linux through `notify::recommended_watcher`).
2. Recursively register all existing source roots.
3. Run catch-up: enumerate candidate files and compare metadata with cursors.
4. Skip a file only when mtime and size match and cursor offset is at/after size.
5. Seek to the cursor offset and consume complete newline-terminated records. A partial final line waits for a later append.
6. Coalesce queued filesystem notifications and resolve only changed paths through `entry_for`.
7. On `SIGINT`/`SIGTERM`, unwatch roots, publish final status, and release the lease.

The loop checks stop signals every 250 ms. Source reads use a 64 KiB buffer; event batches checkpoint at 512 events.

## Commit protocol

For each source delta:

1. Claim the single writer lease.
2. Parse all complete new source lines.
3. Mask/cap and group canonical events by date.
4. Append complete payloads to deterministic daily partitions.
5. Update affected Oko session projections.
6. Record the newline-aligned source cursor through atomic, fsynced publication.
7. Emit per-runtime event/mask counts and release the lease.

The checkpoint follows durable outputs. A crash can replay events, but cannot advance past missing evidence. Oko deduplicates by deterministic event fingerprint. Source shrink returns `source shrank after its last checkpoint; preserve the Lake and use rebuild`; a same-size rewrite returns `source changed without an append; preserve the Lake and use rebuild`.

## Hook closed segments

Closed segments are immutable producer publications. The stream content-checks each output. A pre-existing output with a different digest refuses with `hook segment output conflict: <path>`. Successful output uses temp write, file fsync, rename, and parent fsync; committed segment records make replay idempotent. Legacy mutable hook logs are used only when the closed-segment ready directory is absent.

## Replay recovery

`rebuild --to <empty-path> [--source <runtime>]` obtains a distinct empty destination, enumerates selected source history, and runs the same parse/mask/canonicalize path. It refuses the current root or non-empty destination. It never repairs in place or deletes the preserved Lake.