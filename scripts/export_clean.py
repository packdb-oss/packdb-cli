#!/usr/bin/env python3
# Copyright 2025 PackDB Contributors
#
# Licensed under the Apache License, Version 2.0
"""Fail if repository files contain material that must not be published.

The public ruleset flags credentials, private network addresses, local
filesystem paths, cloud resource identifiers and unapproved e-mail addresses.
Maintainers may pass `--denylist FILE` with additional literal identifiers
(one per line); that list is kept outside the repository and never committed.

An exception names one exact string in one file, with a reason, in
`scripts/export_clean_allow.txt` as `path<TAB>string<TAB>reason`. No rule can be
disabled globally. This script is excluded from its own scan because it must
contain the patterns it looks for.
"""

from __future__ import annotations

import argparse
import re
import subprocess
import sys
from pathlib import Path

RULES: list[tuple[str, re.Pattern[str]]] = [
    ("private key", re.compile(r"-----BEGIN [A-Z ]*PRIVATE KEY-----")),
    ("AWS access key", re.compile(r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b")),
    ("GitHub token", re.compile(r"\b(?:gh[pousr]_[A-Za-z0-9]{30,}|github_pat_[A-Za-z0-9_]{30,})\b")),
    ("Slack token", re.compile(r"\bxox[abprs]-[A-Za-z0-9-]{10,}")),
    ("generic secret assignment", re.compile(
        r"(?i)\b(?:password|passwd|secret|api[_-]?key|access[_-]?token)\b\s*[:=]\s*['\"][^'\"\s]{8,}['\"]")),
    ("cloud resource id", re.compile(r"\bocid1\.[a-z0-9.]+|\barn:aws:[a-z0-9-]+:[a-z0-9-]*:\d{12}:")),
    ("private IPv4 address", re.compile(
        r"\b(?:10(?:\.\d{1,3}){3}|192\.168(?:\.\d{1,3}){2}|172\.(?:1[6-9]|2\d|3[01])(?:\.\d{1,3}){2})\b")),
    ("local filesystem path", re.compile(r"(?:/Users/[A-Za-z0-9._-]+|/home/[A-Za-z0-9._-]+|[A-Za-z]:\\Users\\)")),
    ("internal hostname", re.compile(r"\b[a-z0-9.-]+\.(?:internal|corp|lan|intranet)\b")),
    ("e-mail address", re.compile(r"\b[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}\b")),
]

ALLOWED_EMAILS = {"security@packdb.io", "conduct@packdb.io"}
SELF = "scripts/export_clean.py"
ALLOW_FILE = "scripts/export_clean_allow.txt"


def tracked_files(root: Path) -> list[str]:
    if (root / ".git").exists():
        out = subprocess.run(
            ["git", "ls-files", "-z", "--cached", "--others", "--exclude-standard"],
            cwd=root, check=True, capture_output=True,
        ).stdout.decode()
        return sorted(p for p in out.split("\0") if p)
    return sorted(
        p.relative_to(root).as_posix()
        for p in root.rglob("*")
        if p.is_file() and not any(part in {".git", "target"} for part in p.relative_to(root).parts)
    )


def load_allow(root: Path) -> set[tuple[str, str]]:
    path = root / ALLOW_FILE
    allowed: set[tuple[str, str]] = set()
    if not path.exists():
        return allowed
    for n, line in enumerate(path.read_text().splitlines(), 1):
        if not line.strip() or line.startswith("#"):
            continue
        parts = line.split("\t")
        if len(parts) != 3 or not all(p.strip() for p in parts):
            raise SystemExit(f"{ALLOW_FILE}:{n}: expected path<TAB>string<TAB>reason")
        allowed.add((parts[0], parts[1]))
    return allowed


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--root", type=Path, default=Path.cwd())
    parser.add_argument("--denylist", type=Path, help="private file of literal identifiers")
    args = parser.parse_args()
    root = args.root.resolve()
    allowed = load_allow(root)
    deny = []
    if args.denylist:
        deny = [l.strip() for l in args.denylist.read_text().splitlines() if l.strip() and not l.startswith("#")]
    findings: list[str] = []
    used: set[tuple[str, str]] = set()
    files = tracked_files(root)
    for rel in files:
        if rel == SELF:
            continue
        data = (root / rel).read_bytes()
        if b"\0" in data:
            findings.append(f"{rel}: binary file needs manual review")
            continue
        text = data.decode("utf-8", errors="replace")
        for n, line in enumerate(text.splitlines(), 1):
            for name, pattern in RULES:
                for m in pattern.finditer(line):
                    hit = m.group(0)
                    if name == "e-mail address" and hit.lower() in ALLOWED_EMAILS:
                        continue
                    if (rel, hit) in allowed:
                        used.add((rel, hit))
                        continue
                    findings.append(f"{rel}:{n}: {name}: {hit}")
            for ident in deny:
                if ident in line:
                    # Never echo a confidential identifier into logs.
                    findings.append(f"{rel}:{n}: private denylist match")
    for rel, hit in sorted(allowed - used):
        findings.append(f"{ALLOW_FILE}: stale exception for {rel}: {hit}")
    for f in findings:
        print(f)
    print(f"export-clean: {len(files)} files, {len(findings)} finding(s)")
    return 1 if findings else 0


if __name__ == "__main__":
    sys.exit(main())
