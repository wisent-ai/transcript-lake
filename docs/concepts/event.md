# Concept: canonical event

A canonical event is the smallest durable unit in Transcript Lake: one masked, provider-neutral observation serialized as one NDJSON line.

Its frozen field order is `ts`, `runtime`, `machine`, `session_id`, `project`, `event_type`, `text`, `tool_name`, `model`, `tokens_in`, `tokens_out`, `extra`. Nullable fields preserve absence rather than inventing values. `text` is always a string and `extra` an object.

The seven event types are:

- `user` and `assistant`: conversation turns;
- `thinking`: reasoning material exposed by the runtime;
- `tool_call` and `tool_result`: tool boundary records;
- `meta`: session/runtime metadata that is useful without becoming conversation text;
- `hook_decision`: a Tama policy-hook decision.

The runtime-native `session_id` is the conversation identity. The same spelling from two runtimes is disambiguated by `runtime`. `extra.source_stem_hash` is a one-way bridge to file-oriented indexes without retaining the source filename stem.

Events are immutable after append. They are sensitive even when masked: time, machine, project, tools, model, ordinary text, and short nonmatching secrets can remain.

See [masking](masking.md), [partition](partition.md), and the [full schema](../ingestion-reference.md#raw-to-canonical-transformation).