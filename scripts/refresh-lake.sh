#!/bin/sh
# Canonical logic for the scheduled lake refresh: incremental ingest, then the
# per-session Oko export. Runs the PACKAGED CLI (npm pack + npm install -g .)
# because launchd has no TCC grant for ~/Documents; the loaded LaunchAgent
# (com.wisent.transcript-lake-refresh) executes the mirror of this script at
# ~/.local/bin/transcript-lake-refresh with an nvm-aware PATH. After changing
# the CLI, repack and reinstall before the timer picks the change up.
set -eu

transcript-lake ingest
transcript-lake export-oko
