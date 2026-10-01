#!/usr/bin/env python3
# Copyright 2026 PackDb Contributors
#
# Licensed under the Apache License, Version 2.0
"""Fail when a first-party source file does not carry the Apache 2.0 notice.

Shared, byte for byte, by packdb-sdk, packdb-cli, logql-parser and
traceql-parser; see docs/SOURCE_HEADERS.md for the policy and the update
procedure. Dependency licenses are a separate check (`cargo deny`).

Every tracked file is classified. Rust, Python and shell files -- including an
extensionless script identified by its shebang -- are source and must open with
a comment block, after the shebang if there is one, holding both

    Copyright <year>[-<year>] PackDb Contributors      (or PackDB)
    Licensed under the Apache License, Version 2.0

in that order; the full Apache boilerplate that follows the second line in
some files is accepted as is. Any other file type must be declared as
non-source here: an unclassified type fails, so a new language cannot be
silently skipped.

A file that must not carry this notice (generated, or third-party with its own
notice) is listed by exact path in `scripts/source_header_exceptions.txt` as
`path<TAB>provenance<TAB>reason`. An exception that names an untracked or
non-source file, or a file that already carries the notice, is stale and fails.

    check_source_headers.py              # self-test, then check the repository
    check_source_headers.py --self-test  # controls only
"""

from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path

CHECKER_VERSION = "1"
EXCEPTIONS = "scripts/source_header_exceptions.txt"

# Comment marker of each source language.
SOURCE_SUFFIXES = {".rs": "//", ".py": "#", ".sh": "#", ".bash": "#"}
SHEBANG_LANGUAGES = {"python": "#", "python3": "#", "bash": "#", "sh": "#"}
# Declared non-source: data, configuration, documentation and license texts.
NON_SOURCE_SUFFIXES = {
    ".md", ".toml", ".lock", ".yml", ".yaml", ".json", ".jsonl", ".txt", ".sha256",
}
NON_SOURCE_NAMES = {"LICENSE", "CODEOWNERS", ".gitignore"}

COPYRIGHT = re.compile(r"Copyright \d{4}(-\d{4})? PackD[Bb] Contributors")
APACHE = re.compile(r'Licensed under the Apache License, Version 2\.0( \(the "License"\);)?')
# A license statement that is not the Apache one.
OTHER_LICENSE = re.compile(r"(?i)(SPDX-License-Identifier:|Licensed under|\bLicense:)")


def comment_marker(path: str, text: str) -> str | None:
    """The comment marker if `path` is source, "" if declared non-source, None if unknown."""
    name = Path(path).name
    suffix = Path(path).suffix
    if suffix in SOURCE_SUFFIXES:
        return SOURCE_SUFFIXES[suffix]
    if name in NON_SOURCE_NAMES or suffix in NON_SOURCE_SUFFIXES:
        return ""
    if not suffix and text.startswith("#!"):
        words = text.splitlines()[0][2:].split()
        program = words[-1] if words and Path(words[0]).name == "env" else (words[0] if words else "")
        return SHEBANG_LANGUAGES.get(Path(program).name)
    return None


def leading_comments(text: str, marker: str) -> list[str]:
    """The comment text of the block that opens the file, after any shebang."""
    lines = text.splitlines()
    if lines and re.match(r"#!\s*/", lines[0]):
        lines = lines[1:]
    block = []
    for line in lines:
        # Rust doc comments (`///`, `//!`) document the item, not the file.
        if not line.startswith(marker) or (marker == "//" and line[2:3] in ("/", "!")):
            break
        block.append(line[len(marker):].strip())
    return block


def notice_problem(path: str, text: str, marker: str) -> str | None:
    # `#!/...` only: Rust inner attributes (`#![...]`) also start with `#!`.
    if re.search(r"^#!\s*/", text, re.M) and not re.match(r"#!\s*/", text):
        return "shebang is not the first line"
    block = leading_comments(text, marker)
    copyright_at = next((i for i, line in enumerate(block) if COPYRIGHT.fullmatch(line)), None)
    apache_at = next((i for i, line in enumerate(block) if APACHE.fullmatch(line)), None)
    for line in block:
        if OTHER_LICENSE.search(line) and not APACHE.fullmatch(line):
            return f"names a license other than Apache 2.0: {line!r}"
    if copyright_at is None or apache_at is None or apache_at < copyright_at:
        return "missing the Apache 2.0 notice (Copyright line, then the Apache license line)"
    return None


def parse_exceptions(text: str) -> tuple[dict[str, tuple[str, str]], list[str]]:
    exceptions: dict[str, tuple[str, str]] = {}
    errors = []
    for number, raw in enumerate(text.splitlines(), 1):
        if not raw.strip() or raw.startswith("#"):
            continue
        fields = raw.split("\t")
        where = f"{EXCEPTIONS}:{number}"
        if len(fields) != 3 or not all(f.strip() for f in fields):
            errors.append(f"{where}: expected path<TAB>provenance<TAB>reason, all non-empty")
            continue
        path, provenance, reason = (f.strip() for f in fields)
        if path in exceptions:
            errors.append(f"{where}: duplicate exception for {path}")
        exceptions[path] = (provenance, reason)
    return exceptions, errors


def check(files: dict[str, str], exceptions_text: str) -> tuple[list[str], int]:
    """Problems for the tracked `files` (path -> text), and how many were source."""
    exceptions, problems = parse_exceptions(exceptions_text)
    examined = 0
    for path in sorted(files):
        text = files[path]
        marker = comment_marker(path, text)
        if marker is None:
            problems.append(f"{path}: unclassified file type; declare it as source or non-source "
                            "in scripts/check_source_headers.py (see docs/SOURCE_HEADERS.md)")
            continue
        if not marker:
            if path in exceptions:
                problems.append(f"{path}: stale exception; the file is not source")
            continue
        examined += 1
        problem = notice_problem(path, text, marker)
        if path in exceptions:
            if problem is None:
                problems.append(f"{path}: stale exception; the file carries the notice")
        elif problem:
            problems.append(f"{path}: {problem}")
    for path in sorted(set(exceptions) - set(files)):
        problems.append(f"{path}: stale exception; the file is not tracked")
    if not examined:
        problems.append("no source files examined")
    return problems, examined


SHORT_RS = "// Copyright 2025 PackDb Contributors\n//\n// Licensed under the Apache License, Version 2.0\n\n//! Docs.\n"
LONG_RS = (
    "// Copyright 2026 PackDB Contributors\n//\n"
    '// Licensed under the Apache License, Version 2.0 (the "License");\n'
    "// you may not use this file except in compliance with the License.\n\n//! Docs.\n"
)
PY = "#!/usr/bin/env python3\n# Copyright 2026 PackDb Contributors\n#\n# Licensed under the Apache License, Version 2.0\n\"\"\"Doc.\"\"\"\n"
SH = "#!/usr/bin/env bash\n# Copyright 2025 PackDb Contributors\n# Licensed under the Apache License, Version 2.0\nset -eu\n"


def self_test() -> None:
    def problems(files, exceptions=""):
        return check({"src/ok.rs": SHORT_RS, **files}, exceptions)[0]

    def fails(files, needle, exceptions=""):
        found = problems(files, exceptions)
        assert any(needle in p for p in found), f"expected {needle!r}, got {found}"

    accepted = {
        "src/a.rs": SHORT_RS, "src/b.rs": LONG_RS, "scripts/c.py": PY, "scripts/d.sh": SH,
        "scripts/tool": PY, "README.md": "# x\n", "Cargo.lock": "", "LICENSE": "Apache\n",
        "src/years.rs": SHORT_RS.replace("2025", "2025-2026"),
    }
    assert not problems(accepted), problems(accepted)

    fails({"src/x.rs": "//! Docs only.\n"}, "src/x.rs: missing")
    fails({"src/x.rs": "fn main() {}\n// Copyright 2025 PackDb Contributors\n"
                       "// Licensed under the Apache License, Version 2.0\n"}, "src/x.rs: missing")
    fails({"src/x.rs": "//! Copyright 2025 PackDb Contributors\n"
                       "//! Licensed under the Apache License, Version 2.0\n"}, "src/x.rs: missing")
    fails({"scripts/x.py": "#!/usr/bin/env python3\n\"\"\"Copyright 2025 PackDb Contributors\n"
                           "Licensed under the Apache License, Version 2.0\"\"\"\n"}, "scripts/x.py: missing")
    fails({"src/x.rs": "// Licensed under the Apache License, Version 2.0\n"
                       "// Copyright 2025 PackDb Contributors\n"}, "src/x.rs: missing")
    fails({"src/x.rs": "// Copyright 2025 Someone Else\n// Licensed under the Apache License, Version 2.0\n"},
          "src/x.rs: missing")
    fails({"src/x.rs": "// Portions Copyright 2025 PackDb Contributors\n"
                       "// Licensed under the Apache License, Version 2.0\n"}, "src/x.rs: missing")
    fails({"src/x.rs": "// Copyright 2025 PackDb Contributors\n// Licensed under the MIT License\n"},
          "other than Apache")
    fails({"src/x.rs": SHORT_RS.replace("// Licensed under the Apache License, Version 2.0",
                                        "// SPDX-License-Identifier: MIT")}, "other than Apache")
    fails({"scripts/x.sh": SH.replace("Version 2.0", "Version 1.1")}, "other than Apache")
    # The shebang must stay the first line, or the script stops being executable.
    fails({"scripts/x.sh": SH.split("\n", 1)[1] + "#!/usr/bin/env bash\n"}, "shebang is not the first line")
    fails({"scripts/x.py": "# Copyright 2026 PackDb Contributors\n#!/usr/bin/env python3\n"},
          "shebang is not the first line")
    fails({"scripts/tool": "#!/usr/bin/env python3\nprint()\n"}, "scripts/tool: missing")
    inner_attribute = "#![cfg(feature = \"sql\")]\n" + SHORT_RS
    fails({"tests/x.rs": inner_attribute}, "tests/x.rs: missing")
    assert not problems({"tests/x.rs": SHORT_RS + "#![allow(dead_code)]\n"})

    fails({"web/app.ts": "// x\n"}, "unclassified file type")
    fails({"bin/run": "#!/usr/bin/env node\n"}, "unclassified file type")
    fails({"data.bin": ""}, "unclassified file type")

    third_party = {"vendor/x.rs": "// Copyright Upstream\n// Licensed under the MIT License\n"}
    assert not problems(third_party, "vendor/x.rs\tupstream x v1.0\tthird-party, MIT notice kept\n")
    fails(third_party, "expected path<TAB>provenance<TAB>reason", "vendor/x.rs\tupstream x v1.0\n")
    fails(third_party, "expected path<TAB>provenance<TAB>reason", "vendor/x.rs\t \tno provenance\n")
    fails(third_party, "duplicate exception", "vendor/x.rs\ta\tb\nvendor/x.rs\ta\tb\n")
    fails(third_party, "vendor/x.rs: names a license other than Apache")
    fails({}, "gone.rs: stale exception; the file is not tracked", "gone.rs\tupstream\treason\n")
    fails({}, "src/ok.rs: stale exception; the file carries the notice", "src/ok.rs\tupstream\treason\n")
    fails({"README.md": "x"}, "README.md: stale exception; the file is not source", "README.md\tx\ty\n")

    assert check({"README.md": "x"}, "")[0] == ["no source files examined"]
    print(f"source header checker {CHECKER_VERSION}: negative controls passed")


def tracked_files(root: Path) -> dict[str, str]:
    listed = subprocess.run(["git", "ls-files", "-z"], cwd=root, check=True,
                            capture_output=True).stdout.decode().split("\0")
    files = {}
    for path in filter(None, listed):
        data = (root / path).read_bytes()
        files[path] = data.decode("utf-8", errors="replace")
    return files


def main(argv: list[str]) -> int:
    self_test()
    if argv == ["--self-test"]:
        return 0
    if argv:
        print("usage: check_source_headers.py [--self-test]", file=sys.stderr)
        return 2
    root = Path(__file__).resolve().parents[1]
    exceptions = root / EXCEPTIONS
    if not exceptions.is_file():
        print(f"{EXCEPTIONS} is missing; it lists exceptions, or none", file=sys.stderr)
        return 1
    problems, examined = check(tracked_files(root), exceptions.read_text())
    if problems:
        print("\n".join(problems), file=sys.stderr)
        return 1
    print(f"source headers: {examined} source files carry the Apache 2.0 notice")
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
