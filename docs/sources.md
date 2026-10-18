# The source registry

`transcript-lake adopt` records the vendor transcript roots a Lake follows in
`LAKE_DATA/sources.json`. Discovery never writes it; adoption writes it only
after the source has been validated and ingested through the stream.

## Format

The file is one JSON object followed by a newline:

| Field | Meaning |
|---|---|
| `schemaVersion` | `1`, the only version this Lake reads. Any other version is refused naming the file and the version it expected. |
| `selectedSourceId` | The `id` of the source the Lake follows now. It must be one of `sources`. |
| `sources` | Every adopted source, sorted by `id`, none repeated. |

Each source is `{id, runtime, root, adoptedAt}`: the transcript runtime's
adapter name, the canonical absolute root it was adopted at, and the time of
its first adoption. A runtime the Lake has no adapter for, a relative root,
an id that does not match its runtime and root, an empty source list or a
selected id that is not listed makes the registry invalid; every command that
reads it refuses naming the file and the reason.

## Identity

A source's `id` is `<runtime>:` followed by the first 24 lowercase
hexadecimal digits of the SHA-256 digest of the runtime name, one zero byte
and the root's bytes. Adopting the same root for the same runtime again keeps
the source and its `adoptedAt`, and only selects it.

## Writing

The registry is published whole: a fresh sibling file is written and flushed,
renamed over `sources.json`, and the directory is flushed after it, so a
reader sees the old registry or the new one, never a part. A failed step
names itself and its path; a sibling left behind after a failure is reported
with the reason it could not be removed.
