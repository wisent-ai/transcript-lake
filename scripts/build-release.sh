#!/bin/sh
# Build the attributable release assets for one exact tag: a macOS binary
# archive, its SHA-256 checksum, and a provenance record under dist/.
#
# Refuses a dirty working tree, a tag that does not match the manifest
# version, a non-Darwin host, and any pre-existing output, so a release
# artifact can only ever describe one immutable source revision. Building is
# deliberately not publishing.
set -eu

ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
DIST="$ROOT/dist"
MANIFEST="$ROOT/Cargo.toml"

fail() {
    printf 'release error: %s\n' "$1" >&2
    exit 1
}

[ -r "$MANIFEST" ] || fail "$MANIFEST is not readable"

VERSION=$(sed -n '/^\[package\]/,/^\[/ s/^version *= *"\([^"]*\)".*/\1/p' "$MANIFEST" | sed -n 1p)
PRODUCT=$(sed -n '/^\[\[bin\]\]/,/^\[/ s/^name *= *"\([^"]*\)".*/\1/p' "$MANIFEST" | sed -n 1p)
[ -n "$VERSION" ] || fail 'Cargo.toml declares no package version'
[ -n "$PRODUCT" ] || fail 'Cargo.toml declares no [[bin]] name'

cd "$ROOT"

[ -z "$(git status --porcelain)" ] || fail 'working tree is not clean'
COMMIT=$(git rev-parse HEAD)
TAG=$(git describe --tags --exact-match 2>/dev/null) ||
    fail "expected exact tag v$VERSION, found no tag on the current revision"
[ "$TAG" = "v$VERSION" ] || fail "expected exact tag v$VERSION, found $TAG"

TRIPLE=$(rustc -vV | sed -n 's/^host: //p')
[ -n "$TRIPLE" ] || fail 'rustc did not report a host triple'
case "$TRIPLE" in
    *-apple-darwin) ;;
    *) fail "macOS is the qualified platform; rustc host is $TRIPLE" ;;
esac

NAME="$PRODUCT-$VERSION-$TRIPLE"
ARCHIVE="$NAME.tar.gz"
mkdir -p "$DIST"
for output in "$DIST/$ARCHIVE" "$DIST/$ARCHIVE.sha256" "$DIST/provenance.json"; do
    [ ! -e "$output" ] || fail "refusing to overwrite $output"
done

cargo build --release --locked
BINARY="$ROOT/target/release/$PRODUCT"
[ -x "$BINARY" ] || fail "cargo did not produce $BINARY"
BUILT=$("$BINARY" --version)
[ "$BUILT" = "$VERSION" ] ||
    fail "built binary reports $BUILT, manifest declares $VERSION"

# The binary embeds sql/, so the archive carries it only for the
# TRANSCRIPT_LAKE_SQL override. Product documentation is published at
# https://transcript-lake.wisent.com/docs/.
STAGE="$DIST/$NAME"
rm -rf "$STAGE"
mkdir -p "$STAGE"
cp "$BINARY" "$STAGE/$PRODUCT"
cp "$ROOT/LICENSE" "$ROOT/README.md" "$STAGE/"
cp -R "$ROOT/sql" "$STAGE/"
tar -C "$DIST" -czf "$DIST/$ARCHIVE" "$NAME"
rm -rf "$STAGE"

DIGEST=$(cd "$DIST" && shasum --algorithm 256 "$ARCHIVE" | cut -d' ' -f1)
printf '%s  %s\n' "$DIGEST" "$ARCHIVE" >"$DIST/$ARCHIVE.sha256"

cat >"$DIST/provenance.json" <<PROVENANCE
{
  "product": "$PRODUCT",
  "version": "$VERSION",
  "sourceCommit": "$COMMIT",
  "tag": "$TAG",
  "builtAt": "$(date -u '+%Y-%m-%dT%H:%M:%SZ')",
  "platform": "darwin",
  "architecture": "${TRIPLE%%-*}",
  "artifact": "$ARCHIVE",
  "sha256": "$DIGEST"
}
PROVENANCE

cat "$DIST/provenance.json"
