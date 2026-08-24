# Concept: session

A session is the runtime-native conversation identity aggregated from canonical events. The stable key is `(runtime, session_id)`; project is best-effort metadata, not identity.

The `sessions` view summarizes first/last timestamps, user and assistant counts, tool calls, and reported token counters. `interrupted_sessions` diagnoses a last unanswered user turn or a tool call cut off before another assistant turn. These are query views, not additional stored records.

`show <session-id>` reconstructs selected event types oldest-first. Its default is `user,assistant`; `--include all` adds thinking, tool, meta, and hook records. The footer states rendered versus matched counts so limits never look complete by accident.

The Oko export hashes `runtime + "\n" + session_id` to choose a per-session filename. Oko may index that projection, but ownership of the canonical masked session remains with Transcript Lake and ownership of the raw conversation remains with its vendor runtime.

See [event](event.md), [export](export.md), and [CLI read commands](../cli-reference.md#read-and-analyze).