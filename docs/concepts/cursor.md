# Concept: cursor and writer lease

A byte cursor records how far Transcript Lake has consumed one append-only source file:

```json
{"mtimeMs": 0.0, "size": 0, "offset": 0}
```

`LAKE_DATA/cursors.json` maps absolute source paths to these records. `offset` is always a complete-line boundary. The stream compares current size/mtime to detect a safe append, unchanged file, shrink, or same-size rewrite.

A checkpoint is published only after canonical partitions and affected Oko session projections succeed. Publication uses a cursor lock, temp file, file fsync, rename, and data-directory fsync. Invalid cursor JSON is refused because silently resetting it could duplicate evidence in existing partitions.

`LAKE_DATA/stream.lock/owner.json` grants one state writer at a time. The atomic lease records host, pid, process start, and token. A live incumbent is a refusal; a provably dead one can be recovered. Labels do not share this lease.

A cursor is resume metadata, not a copy of the vendor transcript and not an event index.

See [ingestion commit protocol](../ingestion-reference.md#commit-protocol).