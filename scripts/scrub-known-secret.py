#!/usr/bin/env python3
"""Replace a credential literal that is already stored in a Lake with the marker
the masker would have written for it.

The masker protects new events; it cannot reach the ones already committed. An
operating-system password reached tool calls before the credential class existed,
so it sits in clear text in partitions and Oko projections that are otherwise
append-only, and every agent that greps history rediscovers it. This script is the
one supported way to remove such a literal after the fact.

Behaviour worth knowing before running it:

- Preview is the default. `--apply` is the only way to change a byte.
- The replacement is exactly `[masked:credential:<chars>:<sha256[:8]>]`, the
  spelling `src/redact.rs` produces, so every existing reader keeps working and a
  fingerprint still correlates reuse of the same value across events.
- Idempotent: the marker contains none of the literal, so a second run finds
  nothing. Re-running after the streamer re-imports an old transcript is expected.
- Nothing else changes. Only the literal's bytes are replaced; every other byte,
  every line boundary, and the file's mode are preserved, and a rewritten line
  must still parse as JSON or the whole file is refused.
- `--apply` takes the same exclusive writer lease `stream.lock` that the streamer
  takes for a commit, so a rewrite cannot interleave with an append.
- Counting comes first and is reported per literal, because a literal fed from a
  list is not necessarily a secret: a short or common value would rewrite
  unrelated evidence. A literal found in more than `--max-files` files is refused
  rather than applied, so a mistake in the list costs a report instead of the
  archive.
- The literal never appears in a command line, in output, or in this file: it is
  read from a file or from standard input, and only its length and fingerprint are
  reported.
- `--since <epoch-seconds>` scans only files modified since then, which is what
  makes a scheduled run cost what the streamer wrote rather than the whole archive.
  Partitions are append-only, so a file untouched since the last pass cannot have
  gained a secret.

Usage:
  scrub-known-secret.py --secret-file <path|-> [--data-dir <lake>] [--apply]
                        [--max-files <n>] [--since <epoch-seconds>]
                        [--remove-secret-file]
"""

import hashlib
import json
import os
import pathlib
import re
import subprocess
import sys
import time
import uuid

NONE = None
ZERO = len("")
FP_LEN = len("a" * 8)
# How long to wait for the writer lease before giving up. Ten seconds was
# enough while the only other writer was a streamer publishing a commit per
# batch; it is not enough during a backfill, where `catch_up` holds the lease
# for as long as the walk takes and a scheduled scrub would then do nothing,
# every hour, for the whole backfill. Waiting is correct and stealing is not:
# a rewrite and a commit must never touch one partition at once. The default
# sits under the scrub unit's hourly interval so at most one run waits at a
# time.
LEASE_WAIT_SECONDS = float(os.environ.get("SCRUB_LEASE_WAIT_SECONDS", 50 * 60))
LEASE_PAUSE_SECONDS = 0.5
LEASE_ATTEMPTS = max(len("a" * 20), int(LEASE_WAIT_SECONDS / LEASE_PAUSE_SECONDS))
# Per-line JSON for the append-only stores, one JSON document for the rest. A
# suffix that is neither is rewritten without a structural claim.
NDJSON_SUFFIXES = (".ndjson", ".jsonl")
JSON_SUFFIXES = (".json",)
SKIP_DIR_NAMES = ("stream.lock",)
# A secret occurs in the conversations that used it. A value present in more files
# than this is either not a secret or so pervasive that replacing it would destroy
# evidence, and either way it must be looked at rather than applied.
MAX_FILES_DEFAULT = len("a" * 1024)
USAGE = (
    "usage: scrub-known-secret.py --secret-file <path|-> [--data-dir <lake>] "
    "[--apply] [--max-files <n>] [--since <epoch-seconds>] [--remove-secret-file]"
)


def marker(value):
    """The masker's marker for one value: class, character count, fingerprint."""
    digest = hashlib.sha256(value.encode("utf-8")).hexdigest()[:FP_LEN]
    return f"[masked:credential:{len(value)}:{digest}]"


def fingerprint(value):
    """What may be printed about a secret: how long it is and which one it is."""
    return f"len={len(value)} sha256[:8]={hashlib.sha256(value.encode('utf-8')).hexdigest()[:FP_LEN]}"


def read_literals(source, remove_after):
    """One literal per line, so a rotation set is scrubbed in one pass."""
    if source == "-":
        raw = sys.stdin.read()
    else:
        path = pathlib.Path(source).expanduser()
        raw = path.read_text(encoding="utf-8")
        if remove_after:
            path.unlink()
    literals = [line for line in raw.split("\n") if line.strip()]
    if not literals:
        raise SystemExit("no literal to scrub: the secret source was empty")
    return literals


def process_identity(pid):
    """`ps -o lstart=`, the same start-time identity the lease owner records."""
    with os.popen(f"/bin/ps -o lstart= -p {int(pid)}") as stream:
        text = stream.read().strip()
    return text or NONE



def owner_is_alive(owner):
    """Whether the claim's process still exists, by the same identity the Lake
    records: pid plus its start time, so a recycled pid is not mistaken for the
    original holder. A claim whose process is gone is abandoned, not held, and
    leaving it in place would stop every future run -- which is exactly what a
    streamer stopped mid-backfill leaves behind."""
    if not isinstance(owner, dict):
        return False
    pid = owner.get("pid")
    if not isinstance(pid, int):
        return True
    try:
        os.kill(pid, ZERO)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    recorded = owner.get("started")
    running = process_identity(pid)
    return recorded is NONE or running is NONE or recorded == running


def acquire_lease(data_dir):
    """The Lake's own exclusive writer lease: build the claim privately, publish
    it with one rename. Deliberately never steals a live claim -- stealing it
    would let a rewrite and a commit touch one partition at once."""
    lock_path = data_dir / "stream.lock"
    pid = os.getpid()
    token = str(uuid.uuid4())
    owner = {
        "host": os.uname().nodename,
        "pid": pid,
        "started": process_identity(pid),
        "token": token,
    }
    for _ in range(LEASE_ATTEMPTS):
        prepared = data_dir / f"stream.lock.claim-{pid}-{uuid.uuid4()}"
        prepared.mkdir(parents=True)
        (prepared / "owner.json").write_text(json.dumps(owner), encoding="utf-8")
        try:
            os.rename(prepared, lock_path)
            return lock_path, token
        except OSError:
            for leftover in prepared.iterdir():
                leftover.unlink()
            prepared.rmdir()
            # A claim whose process no longer exists is abandoned, not held:
            # the Lake's own Rust acquirer removes it, and this must agree, or a
            # streamer stopped mid-backfill would lock every later scrub out
            # forever. A live claim is always waited for, never taken.
            incumbent = NONE
            try:
                incumbent = json.loads((lock_path / "owner.json").read_text(encoding="utf-8"))
            except (OSError, ValueError):
                incumbent = NONE
            if lock_path.exists() and not owner_is_alive(incumbent):
                try:
                    for leftover in lock_path.iterdir():
                        leftover.unlink()
                    lock_path.rmdir()
                    print(f"cleared abandoned lease {json.dumps(incumbent)}")
                    continue
                except OSError:
                    pass
            time.sleep(LEASE_PAUSE_SECONDS)
    held = NONE
    try:
        held = json.loads((lock_path / "owner.json").read_text(encoding="utf-8"))
    except (OSError, ValueError):
        pass
    raise SystemExit(f"state writer lock is held: {json.dumps(held)}")


def release_lease(lock_path, token):
    """Release only a lease this process still owns."""
    try:
        owner = json.loads((lock_path / "owner.json").read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return NONE
    if owner.get("token") != token:
        return NONE
    for leftover in lock_path.iterdir():
        leftover.unlink()
    lock_path.rmdir()
    return NONE


def structural_check(path, text):
    """Which lines of the rewritten text must parse, and whether they do. An
    empty list means the rewrite is structurally sound for this file kind."""
    broken = []
    if path.suffix in NDJSON_SUFFIXES:
        for number, line in enumerate(text.split("\n"), start=1):
            if not line.strip():
                continue
            try:
                json.loads(line)
            except ValueError:
                broken.append(number)
    elif path.suffix in JSON_SUFFIXES:
        try:
            json.loads(text)
        except ValueError:
            broken.append(1)
    return broken


def scanner(literals):
    """One pattern for the whole list. Scanning a file once per literal is fine for
    one password and quadratic for a list fed from a vault: 300 literals over ten
    gigabytes is hours of substring search, while one alternation is one pass."""
    ordered = sorted(literals, key=len, reverse=True)
    return re.compile("|".join(re.escape(literal) for literal in ordered))


def count_file(path, pattern):
    """How many times each literal occurs in one file, and how many lines carry
    one. Absent literals are omitted, so an empty result means untouched."""
    text = path.read_text(encoding="utf-8")
    found = {}
    for match in pattern.finditer(text):
        hit = match.group(ZERO)
        found[hit] = found.get(hit, ZERO) + 1
    if not found:
        return found, ZERO
    # Lines, not `splitlines`: partition lines are newline-delimited and real
    # transcript text contains U+2028, which `splitlines` would also split on.
    lines = sum(1 for line in text.split("\n") if pattern.search(line))
    return found, lines


def rewrite_file(path, pattern, markers):
    """Replace the accepted literals in one file, in one pass. Returns
    (lines_changed, occurrences, broken_lines); nothing is written when the rewrite
    would not parse, so a file is either wholly scrubbed or wholly untouched."""
    original = path.read_text(encoding="utf-8")
    occurrences = ZERO

    def substitute(match):
        nonlocal occurrences
        occurrences += 1
        return markers[match.group(ZERO)]

    updated = pattern.sub(substitute, original)
    if occurrences == ZERO:
        return ZERO, ZERO, []
    before = original.split("\n")
    after = updated.split("\n")
    lines_changed = sum(1 for old, new in zip(before, after) if old != new)
    broken = structural_check(path, updated)
    if broken:
        return lines_changed, occurrences, broken
    # Same directory, so the replacement is atomic on this filesystem, and the
    # mode is carried over: a reader either sees the whole old file or the whole
    # new one, never a partially scrubbed line.
    temp = path.with_name(f"{path.name}.scrub-{os.getpid()}-{uuid.uuid4()}")
    with open(temp, "w", encoding="utf-8") as handle:
        handle.write(updated)
        handle.flush()
        os.fsync(handle.fileno())
    os.chmod(temp, os.stat(path).st_mode & 0o7777)
    os.replace(temp, path)
    return lines_changed, occurrences, []


def lake_files(data_dir, since):
    """Every regular file the Lake owns, minus the lease directory. `since` keeps a
    standing run proportional to what the streamer wrote: partitions are only ever
    appended to, so a file untouched since the last pass cannot have gained a
    secret. Zero means every file."""
    for path in sorted(data_dir.rglob("*")):
        if any(part in SKIP_DIR_NAMES for part in path.parts):
            continue
        if not path.is_file():
            continue
        if since and path.stat().st_mtime < since:
            continue
        yield path


def candidate_files(data_dir, literals, since):
    """The subset of Lake files that could contain any literal.

    A vault-sourced run carries hundreds of literals, and Python's `re` walks a
    seven-gigabyte archive against a several-hundred-branch alternation at a
    pace measured in hours -- a standing hourly repair that never finishes is
    not a repair. `grep -F` does the same search with a C multi-pattern matcher
    in minutes, so it decides WHICH files to open and the Python pass, which is
    the part that must be exact and idempotent, then runs over those alone.

    Falls back to every file when grep is unavailable or reports an error other
    than "no matches", because a fast path that silently narrows the sweep would
    be worse than a slow one.
    """
    everything = list(lake_files(data_dir, since))
    if not literals:
        return everything
    patterns = "\n".join(literals)
    try:
        found = subprocess.run(
            # -I skips binary files and -s swallows unreadable ones: without
            # them a single odd file turns the whole prefilter into exit 2, the
            # fallback engages, and the run silently becomes the hour-long walk
            # this exists to avoid.
            ["/usr/bin/grep", "-F", "-l", "-r", "-I", "-s", "-f", "-", str(data_dir)],
            input=patterns,
            capture_output=True,
            text=True,
            check=False,
        )
    except OSError as error:
        print(f"prefilter unavailable ({error}); scanning every file")
        return everything
    if found.returncode not in (ZERO, len("a")):
        detail = (found.stderr or "").strip().splitlines()[-1:] or ["no detail"]
        print(f"prefilter refused (exit {found.returncode}: {detail[0][:120]}); scanning every file")
        return everything
    named = {pathlib.Path(line) for line in found.stdout.splitlines() if line.strip()}
    print(f"prefilter kept {len(named)} of {len(everything)} files")
    return [path for path in everything if path in named]


def option(argv, name, fallback):
    for index, item in enumerate(argv):
        if item == name and index + 1 < len(argv):
            return argv[index + 1]
    return fallback


def main(argv):
    apply_changes = "--apply" in argv
    remove_after = "--remove-secret-file" in argv
    source = option(argv, "--secret-file", NONE)
    max_files = int(option(argv, "--max-files", MAX_FILES_DEFAULT))
    since = float(option(argv, "--since", ZERO))
    data_dir = pathlib.Path(
        option(
            argv,
            "--data-dir",
            os.environ.get("LAKE_DATA") or str(pathlib.Path.home() / ".transcript-lake"),
        )
    ).expanduser()
    if source is NONE:
        raise SystemExit(USAGE)
    if not data_dir.is_dir():
        raise SystemExit(f"no Lake at {data_dir}")

    literals = read_literals(source, remove_after)
    print(f"lake {data_dir}")
    print(f"mode {'apply' if apply_changes else 'preview (default; pass --apply to write)'}")
    print(f"literals {len(literals)}  max files per literal {max_files}"
          f"  since {'every file' if not since else time.strftime('%Y-%m-%dT%H:%M:%S', time.localtime(since))}")

    # An applying run takes the lease BEFORE it counts. Counting the whole archive
    # against a list of vault values is minutes of IO, and discovering only then
    # that the streamer holds the lease would throw that work away every time; a
    # run either owns the archive for its whole pass or costs nothing.
    lease = NONE
    if apply_changes:
        lease = acquire_lease(data_dir)
        print(f"lease held {lease[0]}")
    try:
        return scrub(data_dir, literals, apply_changes, max_files, since)
    finally:
        if lease is not NONE:
            release_lease(*lease)


def scrub(data_dir, literals, apply_changes, max_files, since):
    """Count every literal, then rewrite what was accepted. The caller owns the
    lease when this writes."""
    # Pass one counts. Nothing is written yet, which is what lets a literal be
    # refused on its own measurements rather than after the damage.
    scanned = ZERO
    unreadable = ZERO
    hits = {}
    file_lines_found = {}
    per_literal_files = {literal: ZERO for literal in literals}
    per_literal_occurrences = {literal: ZERO for literal in literals}
    pattern = scanner(literals)
    for path in candidate_files(data_dir, literals, since):
        scanned += 1
        try:
            found, lines = count_file(path, pattern)
        except (OSError, UnicodeDecodeError):
            unreadable += 1
            continue
        if not found:
            continue
        hits[path] = found
        file_lines_found[path] = lines
        for literal, occurrences in found.items():
            per_literal_files[literal] += 1
            per_literal_occurrences[literal] += occurrences

    accepted = []
    over_cap = []
    for literal in literals:
        files = per_literal_files[literal]
        occurrences = per_literal_occurrences[literal]
        verdict = "absent"
        if files > max_files:
            verdict = "REFUSED over cap"
            over_cap.append(literal)
        elif files:
            verdict = "accepted"
            accepted.append(literal)
        print(f"literal {fingerprint(literal)} files={files} occurrences={occurrences}"
              f" -> {marker(literal)} {verdict}")

    markers = {literal: marker(literal) for literal in accepted}
    accepted_pattern = scanner(accepted) if accepted else NONE
    targets = [path for path, found in hits.items() if any(item in found for item in accepted)]
    print(f"files scanned {scanned}")
    print(f"files skipped as unreadable or binary {unreadable}")
    print(f"files {'to change' if not apply_changes else 'to rewrite'} {len(targets)}")

    files_changed = ZERO
    lines_changed = ZERO
    occurrences = ZERO
    refused = []
    for path in targets:
        if not apply_changes:
            print(f"  would rewrite {path.relative_to(data_dir)}"
                  f" lines={file_lines_found[path]}"
                  f" occurrences={sum(hits[path].get(item, ZERO) for item in accepted)}")
            continue
        try:
            file_lines, file_hits, broken = rewrite_file(path, accepted_pattern, markers)
        except (OSError, UnicodeDecodeError):
            unreadable += 1
            continue
        if broken:
            refused.append((path, broken))
            continue
        files_changed += 1
        lines_changed += file_lines
        occurrences += file_hits
        print(f"  rewrote {path.relative_to(data_dir)}"
              f" lines={file_lines} occurrences={file_hits}")
    for path, broken in refused:
        print(f"  REFUSED {path.relative_to(data_dir)}: rewrite would not parse at lines {broken}")
    if apply_changes:
        print(f"files changed {files_changed}")
        print(f"lines changed {lines_changed}")
        print(f"occurrences replaced {occurrences}")
    else:
        print(f"lines to change {sum(file_lines_found[path] for path in targets)}")
        print(f"occurrences to replace "
              f"{sum(sum(hits[path].get(item, ZERO) for item in accepted) for path in targets)}")
    print(f"literals refused over cap {len(over_cap)}")
    print(f"files refused {len(refused)}")
    return 1 if refused or over_cap else 0


sys.exit(main(sys.argv[1:]))
