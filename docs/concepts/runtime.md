# Concept: runtime and adapter

A runtime is the stable origin namespace attached to every canonical event: `claude`, `codex`, `omp`, `droid`, `kimi`, or `hooks`.

A transcript adapter owns four operations for one vendor format: return existing roots, enumerate candidate sessions, resolve one notified path without rescanning siblings, and construct a stateful line parser. It does not write, watch, mask, or query.

Vendor runtimes retain ownership of raw transcripts. Transcript Lake opens supported files read-only and never follows Claude oversized-result references. Missing roots are normal. Files outside the candidate shape and malformed/torn records are skipped rather than transformed into invented evidence.

`hooks` differs: Tama publishes immutable closed telemetry segments, with a legacy mutable-log fallback. The canonical runtime is still `hooks`, and records become `hook_decision` events.

Runtime selection in CLI flags is exact and validated. Unsupported values fail rather than silently yielding an empty result.

See [supported inputs](../ingestion-reference.md#supported-inputs).