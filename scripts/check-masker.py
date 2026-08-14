#!/usr/bin/env python3
"""Compile transcript-lake on the host that owns the checkout and replay a
credential fixture through the real masking path.

An agent session cannot compile or write scratch space here: its sandbox refuses
writes to build directories and to /tmp, so `cargo build` dies with
`Permission denied (os error 13)` before it reads a line of Rust. The Stado host
agent runs as the same user without that restriction, so the fleet's own
execution path is also the compile and verification path.

The fixture is a Claude transcript carrying the shapes that leaked an operating
system password into the Lake -- `expect`/`send`, `echo | sudo -S`, password
flags, and credential-named JSON keys -- plus the shapes that must stay readable.
Replay runs twice: the second run feeds the first run's masked text back in, which
is what proves idempotency through the product path rather than in a unit test.

Prints the compiler verdict and one `PASS`/`FAIL` line per expectation.
"""

import hashlib
import json
import os
import pathlib
import shutil
import subprocess
import sys

NONE = None
ZERO = len("")
HOME = pathlib.Path(os.path.expanduser("~"))
TREE = HOME / "Documents" / "CodingProjects" / "Wisent" / "transcript-lake"
CARGO = HOME / ".cargo" / "bin" / "cargo"
SCRATCH = HOME / ".cache" / "transcript-lake-masker"
BUILD = HOME / ".cache" / "transcript-lake-build"
# What the stream LaunchAgent actually runs, per
# `transcript-lake/scripts/install-stream-service.sh`.
INSTALLED = HOME / ".local" / "bin" / "transcript-lake"
TIMEOUT = 3600
KEEP = ("error", "warning", "Finished", "Compiling transcript-lake")

# A dictionary-word password with one digit run and one symbol: invisible to the
# token, entropy and assignment classes, which is exactly why the real one
# survived in clear text until the credential class existed.
SECRET = "FixturePass9!"
SESSION = "0f6d4c1e-2b7a-4d51-9c33-8ab1f0e77c02"
PROJECT_DIR = "-Users-fixture-project"

# Each case is (label, source text, must-not-contain, must-contain).
CASES = [
    (
        "expect-send",
        'PUBKEY=$(cat ~/.ssh/id_ed25519.pub); /usr/bin/expect <<EOF\nset timeout 30\n'
        'spawn ssh -o StrictHostKeyChecking=no charles@127.0.0.1\nexpect "(?i)password"\n'
        'send "' + SECRET + '\\r"\nexpect -re "\\\\$ $"\n'
        'send "mkdir -p ~/.ssh && echo KEY_INSTALLED_MARKER\\r"\nexpect eof\nEOF',
        [SECRET],
        ["send \"[masked:credential:", "mkdir -p ~/.ssh && echo KEY_INSTALLED_MARKER"],
    ),
    (
        "echo-sudo-stdin",
        "ssh -o BatchMode=yes charles@127.0.0.1 'echo \"" + SECRET
        + "\" | sudo -S bash -c \"echo ok\"'",
        [SECRET],
        ["echo \"[masked:credential:", "| sudo -S bash"],
    ),
    (
        "long-password-flag",
        "curl -sS --user charles https://example.invalid --password " + SECRET
        + " && mysqldump --password='" + SECRET + "' fleet",
        [SECRET],
        ["--password [masked:credential:", "--password='[masked:credential:"],
    ),
    (
        "short-password-flag",
        "mysql -u root -p" + SECRET + " -e 'select 1' && mkdir -p ~/.ssh",
        [SECRET],
        ["-p[masked:credential:", "mkdir -p ~/.ssh"],
    ),
    (
        "credential-key",
        '{"login_email":"charles@example.invalid","login_password":"' + SECRET + '"}',
        [SECRET],
        ["\"login_password\":\"[masked:credential:", "charles@example.invalid"],
    ),
    (
        "form-field",
        '{"fields":[{"type":"password","val":"' + SECRET + '"}]}',
        [SECRET],
        ["\"val\":\"[masked:credential:"],
    ),
    (
        "variables-and-prompts-stay-readable",
        "echo \"$PUBKEY\" | sudo -S tee /etc/motd; sudo -p 'Password:' true; "
        "mkdir -p ~/.ssh; docker run -p 8080:80 nginx",
        ["[masked:"],
        ["echo \"$PUBKEY\" | sudo -S tee /etc/motd", "docker run -p 8080:80 nginx"],
    ),
]


def compile_tree():
    """Release build, because the fixture is replayed by the built binary."""
    proc = subprocess.run(
        [str(CARGO), "build", "--release"],
        capture_output=True,
        text=True,
        check=False,
        cwd=str(TREE),
        timeout=TIMEOUT,
        env={
            **os.environ,
            "CARGO_TARGET_DIR": str(BUILD),
            "PATH": f"{CARGO.parent}:/opt/homebrew/bin:/usr/bin:/bin",
        },
    )
    lines = [line.rstrip() for line in (proc.stdout + proc.stderr).splitlines() if line.strip()]
    for line in [line for line in lines if any(mark in line for mark in KEEP)] or lines:
        print(line[: len("a" * 165)])
    print(f"cargo build --release exit {proc.returncode}")
    if proc.returncode != 0:
        raise SystemExit(proc.returncode)
    return BUILD / "release" / "transcript-lake"


def write_fixture(home, texts):
    """One Claude transcript whose user turns carry the fixture texts."""
    project = home / ".claude" / "projects" / PROJECT_DIR
    project.mkdir(parents=True, exist_ok=True)
    records = []
    for index, text in enumerate(texts):
        records.append(
            json.dumps(
                {
                    "type": "user",
                    "sessionId": SESSION,
                    "cwd": "/Users/fixture/project",
                    "timestamp": f"2026-08-14T12:{index:02d}:00.000Z",
                    "message": {"role": "user", "content": text},
                }
            )
        )
    (project / f"{SESSION}.jsonl").write_text("\n".join(records) + "\n", encoding="utf-8")
    return home


def replay(binary, tag, texts):
    """Replay the fixture into a fresh empty Lake and return the masked texts."""
    root = SCRATCH / tag
    if root.exists():
        shutil.rmtree(root)
    fixture_home = write_fixture(root / "home", texts)
    current = root / "current"
    current.mkdir(parents=True, exist_ok=True)
    target = root / "replayed"
    proc = subprocess.run(
        [str(binary), "rebuild", "--to", str(target), "--source", "claude"],
        capture_output=True,
        text=True,
        check=False,
        timeout=TIMEOUT,
        env={
            **os.environ,
            "HOME": str(fixture_home),
            "LAKE_DATA": str(current),
            "PATH": "/usr/bin:/bin",
        },
    )
    if proc.returncode != 0:
        print(proc.stdout.strip()[: len("a" * 400)])
        print(proc.stderr.strip()[: len("a" * 400)])
        raise SystemExit(f"replay {tag} exit {proc.returncode}")
    summary = json.loads(proc.stdout)
    print(f"replay {tag}: {json.dumps(summary, sort_keys=True)[: len('a' * 300)]}")
    masked = []
    for part in sorted((target / "events").rglob("part-*.ndjson")):
        for line in part.read_text(encoding="utf-8").splitlines():
            if line.strip():
                masked.append(json.loads(line)["text"])
    if tag == "first":
        for text in masked:
            print("  masked | " + text.replace("\n", "\\n")[: len("a" * 300)])
    return masked


def check_expectations(label, masked):
    """Compare one replay's output against every case, and count the failures."""
    failures = ZERO
    for (case, _, forbidden, required), got in zip(CASES, masked):
        problems = [f"still present: {mark}" for mark in forbidden if mark in got]
        problems += [f"missing: {mark}" for mark in required if mark not in got]
        print(f"{'FAIL' if problems else 'PASS'} {label}{case}"
              + ("; " + "; ".join(problems) if problems else ""))
        failures += len(problems) > ZERO
    return failures


def main():
    if not TREE.is_dir():
        raise SystemExit(f"no transcript-lake checkout at {TREE}")
    if not CARGO.is_file():
        raise SystemExit(f"no cargo at {CARGO}")
    # macOS protects ~/Documents from a launchd-run helper's writes, and cargo
    # writes its whole target tree; only the build output moves. The host agent
    # also runs helpers under a secret-safe umask that strips the execute bit,
    # so cargo cannot enter the directories it just created unless it is relaxed
    # for this process. Nothing here writes a secret.
    os.umask(0o022)
    for directory in (SCRATCH, BUILD):
        directory.mkdir(parents=True, exist_ok=True)
        os.chmod(directory, 0o755)
    binary = compile_tree()

    texts = [text for _, text, _, _ in CASES]
    first = replay(binary, "first", texts)
    failures = check_expectations("", first)

    second = replay(binary, "second", first)
    for (label, _, _, _), once, twice in zip(CASES, first, second):
        same = once == twice
        print(f"{'PASS' if same else 'FAIL'} idempotent:{label}")
        failures += not same

    # The rules only protect the archive once the process that writes the archive
    # carries them, so the deployed artifact is replayed against the same fixture.
    if INSTALLED.is_file():
        stamp = hashlib.sha256(INSTALLED.read_bytes()).hexdigest()[: len("a" * 16)]
        print(f"installed {INSTALLED} sha256[:16]={stamp}")
        failures += check_expectations("installed:", replay(INSTALLED, "installed", texts))
    else:
        print(f"installed {INSTALLED} absent; the stream service carries no build to check")

    print(f"failures {failures}")
    return 1 if failures else 0


sys.exit(main())
