# Synthetic example: masking audit

**Status:** executed 2026-08-24. **Risk:** read-only inspection of an isolated synthetic Lake; optional derived local mutation for compact. **No real transcripts.**

Prepare a fixture containing three invented values that intentionally match assignment, provider-token, and guarded-entropy classes. Stream it into a directory created only for the example.

Inspect the canonical partition and confirm marker shapes, not hard-coded fingerprints:

```text
[masked:assignment:<length>:<8-hex>]
[masked:token:<length>:<8-hex>]
[masked:entropy:<length>:<8-hex>]
```

Search the complete isolated `LAKE_DATA` tree for each exact invented plaintext literal. Success requires zero matches in partitions, cursors, stream status, Oko exports, and any Parquet mirror created after masking. The retained run produced assignment length 37, token length 36, and entropy length 44, with zero literal matches.

With DuckDB 1.5.x:

```sh
transcript-lake compact --source claude
transcript-lake clean --target parquet
```

The executed compact converted a 3,352-byte NDJSON partition to a 5,122-byte Parquet file. The clean command is a preview; add `--apply` only for the isolated directory.

Failure paths:

- Plaintext match: preserve the synthetic fixture and output; stop using the build and report the exact synthetic reproducer.
- Missing marker: confirm the fixture actually satisfies the documented pattern and newline format; do not weaken the guarantee claim.
- DuckDB failure: masking evidence remains valid in canonical NDJSON; repair optional DuckDB separately.

See [the retained walkthrough](../../docs/walkthrough-masking-audit.md) and [masking guarantees](../../docs/masking-guarantees.md).