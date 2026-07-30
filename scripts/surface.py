#!/usr/bin/env python3
"""Print Transcript Lake's observable distribution surface without importing it.

The surface contains the installed executable, every command and flag advertised
by help, and every runtime identifier represented by an adapter. Removing any
of these names breaks a user, script, stored partition selector, or integration.
Stored-schema changes that names alone cannot express require an explicit
breakage declaration when the shared Wisent AutoVersion rule is invoked.
"""

import json
import re
from pathlib import Path


ROOT = Path(__file__).resolve().parent.parent
CLI = ROOT / "src" / "cli.mjs"
PACKAGE = ROOT / "package.json"
ADAPTERS = ROOT / "src" / "adapters"


def refuse(message: str) -> None:
    raise SystemExit("surface error: " + message)


def main() -> None:
    try:
        package = json.loads(PACKAGE.read_text(encoding="utf-8"))
        source = CLI.read_text(encoding="utf-8")
    except (OSError, json.JSONDecodeError) as error:
        refuse(str(error))

    bins = package.get("bin")
    if not isinstance(bins, dict) or bins.get("transcript-lake") != "src/cli.mjs":
        refuse("package bin must map transcript-lake to src/cli.mjs")

    usage = re.search(r"const USAGE = \[(?P<body>.*?)\]\.join", source, re.DOTALL)
    commands_block = re.search(
        r"const COMMANDS = \{(?P<body>.*?)^\};",
        source,
        re.DOTALL | re.MULTILINE,
    )
    if usage is None or commands_block is None:
        refuse("CLI help or command registry did not parse")

    advertised = set(
        re.findall(r"^\s*'  ([a-z][a-z-]*)(?:\s|\[|\")", usage.group("body"), re.MULTILINE)
    )
    registered = set()
    for quoted, bare in re.findall(
        r"^\s*(?:'([^']+)'|([a-z][a-z-]*))\s*:",
        commands_block.group("body"),
        re.MULTILINE,
    ):
        registered.add(quoted or bare)
    if not advertised or advertised != registered:
        refuse(
            "advertised commands " + repr(sorted(advertised))
            + " differ from registered commands " + repr(sorted(registered))
        )

    flags = set(re.findall(r"--[a-z][a-z-]*", usage.group("body")))
    if not flags:
        refuse("CLI help advertises no flags")

    try:
        runtimes = {path.stem for path in ADAPTERS.glob("*.mjs") if path.is_file()}
    except OSError as error:
        refuse(str(error))
    if not runtimes:
        refuse("no runtime adapters found")
    runtimes.add("hooks")

    surface = {"executable:transcript-lake"}
    surface.update("command:" + name for name in advertised)
    surface.update("flag:" + name for name in flags)
    surface.update("runtime:" + name for name in runtimes)
    print(json.dumps({"surface": sorted(surface)}, indent=len("  ")))


if __name__ == "__main__":
    main()
