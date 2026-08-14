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
- The literal never appears in a command line, in output, or in this file: it is
  read from a file or from standard input, and only its length and fingerprint are
  reported.

Usage:
  scrub-known-secret.py --secret-file <path|-> [--data-dir <lake>] [--apply]
                        [--remove-secret-file]
"""

import hashlib
import json
import os
import pathlib
import sys
import time
import uuid

NONE = None
ZERO = len("")
FP_LEN = len("a" * 8)
# The streamer publishes a commit in well under a second, so a lease it holds
# right now is gone within a handful of retries; a lease still held after this
# many attempts belongs to a long operation and must not be stolen.
LEASE_ATTEMPTS = len("a" * 20)
LEASE_PAUSE_SECONDS = 0.5
# Per-line JSON for the append-only stores, one JSON document for the rest. A
# suffix that is neither is rewritten without a structural claim.
NDJSON_SUFFIXES = (".ndjson", ".jsonl")
JSON_SUFFIXES = (".json",)
SKIP_DIR_NAMES = ("stream.lock",)
USAGE = (
    "usage: scrub-known-secret.py --secret-file <path|-> [--data-dir <lake>] "
    "[--apply] [--remove-secret-file]"
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


def scrub_file(path, replacements, apply_changes):
    """Replace every literal in one file. Returns (lines_changed, occurrences,
    broken_lines); nothing is written when the rewrite would not parse."""
    original = path.read_text(encoding="utf-8")
    updated = original
    occurrences = ZERO
    for literal, replacement in replacements:
        occurrences += updated.count(literal)
        updated = updated.replace(literal, replacement)
    if occurrences == ZERO:
        return ZERO, ZERO, []
    before = original.split("\n")
    after = updated.split("\n")
    lines_changed = sum(1 for old, new in zip(before, after) if old != new)
    broken = structural_check(path, updated)
    if broken:
        return lines_changed, occurrences, broken
    if apply_changes:
        # Same directory, so the replacement is atomic on this filesystem, and
        # the mode is carried over: a reader either sees the whole old file or
        # the whole new one, never a partially scrubbed line.
        temp = path.with_name(f"{path.name}.scrub-{os.getpid()}-{uuid.uuid4()}")
        with open(temp, "w", encoding="utf-8") as handle:
            handle.write(updated)
            handle.flush()
            os.fsync(handle.fileno())
        os.chmod(temp, os.stat(path).st_mode & 0o7777)
        os.replace(temp, path)
    return lines_changed, occurrences, []


def main(argv):
    apply_changes = "--apply" in argv
    remove_after = "--remove-secret-file" in argv
    source = NONE
    data_dir = pathlib.Path(
        os.environ.get("LAKE_DATA") or (pathlib.Path.home() / ".transcript-lake")
    )
    for index, item in enumerate(argv):
        if item == "--secret-file" and index + 1 < len(argv):
            source = argv[index + 1]
        if item == "--data-dir" and index + 1 < len(argv):
            data_dir = pathlib.Path(argv[index + 1]).expanduser()
    if source is NONE:
        raise SystemExit(USAGE)
    if not data_dir.is_dir():
        raise SystemExit(f"no Lake at {data_dir}")

    literals = read_literals(source, remove_after)
    replacements = [(literal, marker(literal)) for literal in literals]
    print(f"lake {data_dir}")
    print(f"mode {'apply' if apply_changes else 'preview (default; pass --apply to write)'}")
    for literal, replacement in replacements:
        print(f"literal {fingerprint(literal)} -> {replacement}")

    lease = NONE
    if apply_changes:
        lease = acquire_lease(data_dir)
        print(f"lease held {lease[0]}")
    try:
        scanned = ZERO
        unreadable = ZERO
        files_changed = ZERO
        lines_changed = ZERO
        occurrences = ZERO
        refused = []
        for path in sorted(data_dir.rglob("*")):
            if not path.is_file() or any(part in SKIP_DIR_NAMES for part in path.parts):
                continue
            scanned += 1
            try:
                file_lines, file_hits, broken = scrub_file(path, replacements, apply_changes)
            except (OSError, UnicodeDecodeError):
                unreadable += 1
                continue
            if file_hits == ZERO:
                continue
            if broken:
                refused.append((path, broken))
                continue
            files_changed += 1
            lines_changed += file_lines
            occurrences += file_hits
            print(f"  {'rewrote' if apply_changes else 'would rewrite'} {path.relative_to(data_dir)}"
                  f" lines={file_lines} occurrences={file_hits}")
    finally:
        if lease is not NONE:
            release_lease(*lease)
    for path, broken in refused:
        print(f"  REFUSED {path.relative_to(data_dir)}: rewrite would not parse at lines {broken}")
    print(f"files scanned {scanned}")
    print(f"files skipped as unreadable or binary {unreadable}")
    print(f"files {'changed' if apply_changes else 'to change'} {files_changed}")
    print(f"lines {'changed' if apply_changes else 'to change'} {lines_changed}")
    print(f"occurrences {'replaced' if apply_changes else 'to replace'} {occurrences}")
    print(f"files refused {len(refused)}")
    return 1 if refused else 0


sys.exit(main(sys.argv[1:]))
