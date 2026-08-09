#!/bin/sh
# Print Transcript Lake's observable distribution surface without running it.
#
# The surface contains the installed executable, every command and flag
# advertised by help, and every runtime identifier represented by an adapter.
# Removing any of these names breaks a user, script, stored partition
# selector, or integration. Stored-schema changes that names alone cannot
# express require an explicit breakage declaration when the shared Wisent
# AutoVersion rule is invoked.
set -eu

ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
MAIN="$ROOT/src/main.rs"
MANIFEST="$ROOT/Cargo.toml"
ADAPTERS="$ROOT/src/adapters"

refuse() {
    printf 'surface error: %s\n' "$1" >&2
    exit 1
}

[ -r "$MAIN" ] || refuse "$MAIN is not readable"
[ -r "$MANIFEST" ] || refuse "$MANIFEST is not readable"

EXECUTABLE=$(sed -n '/^\[\[bin\]\]/,/^\[/ s/^name *= *"\([^"]*\)".*/\1/p' "$MANIFEST" | sed -n 1p)
BINPATH=$(sed -n '/^\[\[bin\]\]/,/^\[/ s/^path *= *"\([^"]*\)".*/\1/p' "$MANIFEST" | sed -n 1p)
[ "$EXECUTABLE" = "transcript-lake" ] ||
    refuse "manifest [[bin]] must name transcript-lake, found ${EXECUTABLE:-none}"
[ "$BINPATH" = "src/main.rs" ] ||
    refuse "manifest [[bin]] must point transcript-lake at src/main.rs, found ${BINPATH:-none}"

# `command_help` is what help advertises; `dispatch` is what the router
# actually routes. A name in one and not the other is a surface defect.
HELP_BLOCK=$(sed -n '/^pub fn command_help/,/^}/p' "$MAIN")
DISPATCH_BLOCK=$(sed -n '/^fn dispatch/,/^}/p' "$MAIN")
USAGE_BLOCK=$(sed -n '/^pub const USAGE/,/^);/p' "$MAIN")
[ -n "$HELP_BLOCK" ] || refuse 'CLI command help did not parse'
[ -n "$DISPATCH_BLOCK" ] || refuse 'CLI command registry did not parse'
[ -n "$USAGE_BLOCK" ] || refuse 'CLI usage text did not parse'

arms() {
    sed -n 's/^ *"\([a-z][a-z-]*\)" *=>.*/\1/p' | sort -u
}

ADVERTISED=$(printf '%s\n' "$HELP_BLOCK" | arms)
REGISTERED=$(printf '%s\n' "$DISPATCH_BLOCK" | arms)
[ -n "$ADVERTISED" ] || refuse 'CLI help advertises no commands'
[ "$ADVERTISED" = "$REGISTERED" ] || refuse "advertised commands
$ADVERTISED
differ from registered commands
$REGISTERED"

FLAGS=$(printf '%s\n%s\n' "$USAGE_BLOCK" "$HELP_BLOCK" |
    grep -o -- '--[a-z][a-z-]*' | sort -u)
[ -n "$FLAGS" ] || refuse 'CLI help advertises no flags'

[ -d "$ADAPTERS" ] || refuse "$ADAPTERS is not a directory"
RUNTIMES=$(
    for module in "$ADAPTERS"/*.rs; do
        [ -f "$module" ] || continue
        stem=$(basename "$module" .rs)
        [ "$stem" = "mod" ] || printf '%s\n' "$stem"
    done
)
[ -n "$RUNTIMES" ] || refuse 'no runtime adapters found'
# `hooks` is an inline driver source rather than an adapter module, but it is
# a public runtime identifier all the same.
RUNTIMES=$(printf '%s\nhooks\n' "$RUNTIMES" | sort -u)

# Word splitting on the collected names is deliberate: each list is one
# identifier per line and identifiers never contain whitespace.
SURFACE=$(
    printf 'executable:%s\n' "$EXECUTABLE"
    printf 'command:%s\n' $ADVERTISED
    printf 'flag:%s\n' $FLAGS
    printf 'runtime:%s\n' $RUNTIMES
)

printf '{\n  "surface": [\n'
printf '%s\n' "$SURFACE" | sort -u | sed -e 's/.*/    "&",/' -e '$ s/,$//'
printf '  ]\n}\n'
