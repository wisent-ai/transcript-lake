# Masking guarantees and limits

Masking is the boundary between adapter output and every durable Lake event. Adapters parse unmasked vendor records in memory; only the stream canonicalizer may write partitions or Oko projections.

## What never leaves the masking boundary

For a detected hit, none of the matched plaintext bytes, including a prefix or suffix, are written to `LAKE_DATA`. The complete hit becomes:

```text
[masked:<class>:<character-count>:<sha256-prefix>]
```

The fingerprint is the first eight lowercase hex characters of SHA-256 over the complete hit. It is deterministic and non-reversible, but permits equality correlation: the same secret produces the same marker fingerprint.

Masking is applied in this order:

1. **assignment** — `\b[A-Z][A-Z0-9_]{2,}=[A-Za-z0-9+/=_-]{16,}`; the complete name, equals sign, and value are removed.
2. **token** — `\b[a-z]{2,7}-[A-Za-z0-9_-]{20,}`; the complete provider-shaped token is removed.
3. **entropy** — candidate runs of at least 40 characters from `[A-Za-z0-9+/=_-]`, accepted only with at least 16 distinct characters and at least three of lowercase, uppercase, digit, and symbol groups.

Assignments run first so a whole `NAME=value` becomes one marker. Provider tokens run before the generic entropy rule. Existing markers cannot match the hit alphabets, making the transform idempotent.

The stream applies the same transform to `text` and retained strings in `extra`, then caps each text/string value at 65,536 UTF-16 units and JSON nesting beyond depth four. Oversized Claude tool results are referenced, never followed. Kimi configuration and system-prompt records never contribute their text.

## Demonstrated with synthetic evidence

The executed fixture produced:

```text
[masked:assignment:37:5be474f8]
[masked:token:36:e3bebba1]
[masked:entropy:44:d0fa7c09]
```

The same synthetic assignment appeared in both a user message and tool result with the same fingerprint. A literal scan for the three invented plaintext values returned zero matches throughout that isolated Lake, including the canonical partition and Oko export.

## What is not guaranteed

Masking is pattern-based, not semantic secret detection and not anonymization.

The following may remain:

- ordinary prose and source code;
- short passwords, tokens outside the supported alphabets, bearer strings without the documented shape, or secrets split by punctuation/whitespace;
- timestamps, runtime, machine hostname, session id, absolute project path, event type, tool name, model, token counters, and selected source-specific metadata;
- the marker class, original character count, and stable equality fingerprint;
- operator labels and notes. Labels are a separate append-only operator store and are not passed through the event masker.

Therefore:

- Treat all Lake roots as sensitive local data.
- Never publish a Lake based only on the word “masked.”
- Rotate any credential pasted into a transcript; masking the archive does not revoke it.
- Use synthetic fixtures for documentation, demonstrations, bug reports, and qualification.

## Durability relationship

The writer lease covers partition append, affected Oko session projection, and cursor checkpoint. The cursor cannot advance ahead of durable masked output. A failure before checkpoint causes replay rather than skipped source bytes. Hook closed segments use temp-file write, file `fsync`, rename, and parent-directory `fsync`; normal transcript partitions append complete event batches, and cursor publication is atomic.

See [masking concept](concepts/masking.md), [ingestion reference](ingestion-reference.md), and [runbook](runbook.md).