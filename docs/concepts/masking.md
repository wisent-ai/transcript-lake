# Concept: masking

Masking is a deterministic string transformation applied after vendor parsing and before durable Lake output. It replaces each complete detected assignment, provider-shaped token, or guarded high-entropy run with `[masked:<class>:<length>:<fingerprint>]`.

The marker preserves only class, character count, and an eight-hex SHA-256 prefix. The same plaintext maps to the same marker, enabling reuse correlation without retaining the value. Existing markers do not rematch, so masking is idempotent.

Masking is not encryption, secret storage, revocation, or anonymization. It has no key and cannot recover a value, but it also does not recognize arbitrary secret semantics. Operators must secure the Lake and rotate credentials disclosed in source transcripts.

There is one masking boundary: adapters emit raw in-memory strings; the stream canonicalizer masks them; partitions and Oko projections receive only canonical masked strings. Operator labels are a separate store and are not masked.

See [guarantees and limits](../masking-guarantees.md).