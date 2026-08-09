#!/bin/sh
# Install the self-contained binary and one event-driven macOS LaunchAgent.
# There is no timer, periodic refresh, or secondary export process.
set -eu

ROOT=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
PREFIX=${TRANSCRIPT_LAKE_PREFIX:-"$HOME/.local"}
BIN="$PREFIX/bin/transcript-lake"
DATA=${LAKE_DATA:-"$HOME/.transcript-lake"}
AGENTS="$HOME/Library/LaunchAgents"
LOGS="$HOME/Library/Logs"
LABEL=com.wisent.transcript-lake-stream
PLIST="$AGENTS/$LABEL.plist"
DOMAIN="gui/$(id -u)"

cargo install --path "$ROOT" --root "$PREFIX" --locked --force
mkdir -p "$AGENTS" "$LOGS"

# Clean cutover from the removed timer and watch-loop services.
for obsolete in com.wisent.transcript-lake-refresh com.wisent.transcript-lake-watch; do
    launchctl bootout "$DOMAIN/$obsolete" >/dev/null 2>&1 || true
    rm -f "$AGENTS/$obsolete.plist" "$AGENTS/$obsolete.plist.disabled"
done
rm -f "$PREFIX/bin/transcript-lake-refresh" "$PREFIX/bin/transcript-lake-watch"
rm -f "$DATA/last-ingest.json" "$DATA/ingest.lock"

cat >"$PLIST" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>$LABEL</string>
    <key>ProgramArguments</key>
    <array>
        <string>$BIN</string>
        <string>stream</string>
        <string>--json</string>
    </array>
    <key>EnvironmentVariables</key>
    <dict>
        <key>LAKE_DATA</key>
        <string>$DATA</string>
    </dict>
    <key>KeepAlive</key>
    <true/>
    <key>RunAtLoad</key>
    <true/>
    <key>ProcessType</key>
    <string>Background</string>
    <key>StandardOutPath</key>
    <string>$LOGS/transcript-lake-stream.log</string>
    <key>StandardErrorPath</key>
    <string>$LOGS/transcript-lake-stream.log</string>
</dict>
</plist>
EOF

launchctl bootout "$DOMAIN/$LABEL" >/dev/null 2>&1 || true
attempt=0
while ! launchctl bootstrap "$DOMAIN" "$PLIST" >/dev/null 2>&1; do
    attempt=$((attempt + 1))
    if [ "$attempt" -ge 50 ]; then
        launchctl bootstrap "$DOMAIN" "$PLIST"
        exit 1
    fi
    sleep 0.1
done
