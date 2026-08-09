#!/bin/sh
# Canonical logic for the scheduled lake refresh: incremental ingest, then the
# per-session Oko export. Runs the INSTALLED binary rather than a checkout,
# because launchd has no TCC grant for ~/Documents:
#   cargo install --path . --root "$HOME/.local"
# puts it at ~/.local/bin/transcript-lake, and the views are compiled into the
# binary, so nothing outside that one file has to be readable. The loaded
# LaunchAgent (com.wisent.transcript-lake-refresh) executes the mirror of this
# script at ~/.local/bin/transcript-lake-refresh. After changing the CLI,
# reinstall before the timer picks the change up.
set -eu

PATH="$HOME/.local/bin:/opt/homebrew/bin:/usr/bin:/bin"
export PATH

transcript-lake ingest
transcript-lake export-oko
