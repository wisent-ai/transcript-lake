# Concept: label

A label is an operator-owned aspect/value annotation over an existing Lake session. Each assignment appends one fsynced line to `LAKE_DATA/labels/labels.ndjson` with timestamp, session id, runtime, normalized aspect, value, optional note, and provenance source.

Labels do not change canonical events. The SQL `labels` view exposes full history; CLI lists use latest-assignment-wins for each session/aspect pair. Reassigning an aspect appends rather than edits.

Aspects are trimmed and lowercased. Values and notes are trimmed. Provenance is `manual` by default and may be `manual`, `human`, `model`, or `brama`, optionally followed by a `:<detail>` suffix.

Labels are not passed through the event masker. Never put credentials or private transcript excerpts in a label or note. The label store is independent of the event writer lease, so labeling can continue while the stream runs.

`goal label` is a specialized model-produced label over the first masked user prompt; it records model provenance.

See `label add`, `label list`, and `label aspects` in the [CLI reference](../cli-reference.md#labels-and-local-goal-titles).