#!/usr/bin/env python3
# Copyright 2025 PackDB Contributors
#
# Licensed under the Apache License, Version 2.0
"""Per-file line-coverage gate.

Rules:

  C1  Every gated file holds at least THRESHOLD percent line coverage. The
      aggregate is printed for information only; a well-covered file never
      pays for a thin one.
  C2  A gated file with no LCOV record fails as "missing coverage data". It
      is neither scored as 0% (a measurement nobody made) nor skipped (which
      would let a file escape the gate by being left out of the build).
  C3  A file with no executable statements (module declarations, re-exports,
      derived types) produces no LCOV record. It passes only if it is listed in
      `scripts/coverage_declaration_only.txt` with a reason, still has no
      instrumented lines, and contains no `fn`, `impl`, `static` or
      `macro_rules!` item. Unfamiliar content therefore fails closed.

Gated files are the changed paths (or, in full mode, every path) that are Rust
sources under `crates/*/src/`. Tests, examples, scripts and documentation are reported
as out of scope rather than dropped silently.

`--self-test` plants a violation of each rule against synthetic LCOV data and
checks that each one fires, plus a compliant sample that must pass. Without it,
a parser that stopped matching `SF:` records would report green over nothing.
"""

from __future__ import annotations

import argparse
import re
import sys
import tempfile
from dataclasses import dataclass
from pathlib import Path

THRESHOLD = 80.0
GATED_RE = re.compile(r"^crates/[^/]+/src/.+\.rs$")
DECLARATION_LIST = "scripts/coverage_declaration_only.txt"
EXECUTABLE_ITEM = re.compile(r"\b(?:fn|impl|static)\b|macro_rules!")


def load_declaration_only(repo_root: Path) -> dict[str, str]:
    path = repo_root / DECLARATION_LIST
    entries: dict[str, str] = {}
    if not path.exists():
        return entries
    for n, line in enumerate(path.read_text().splitlines(), 1):
        if not line.strip() or line.startswith("#"):
            continue
        parts = line.split("\t")
        if len(parts) != 2 or not parts[1].strip():
            raise SystemExit(f"{DECLARATION_LIST}:{n}: expected path<TAB>reason")
        entries[parts[0].strip()] = parts[1].strip()
    return entries


def strip_comments_and_strings(source: str) -> str:
    source = re.sub(r"/\*.*?\*/", " ", source, flags=re.S)
    source = re.sub(r"//[^\n]*", " ", source)
    return re.sub(r'"(?:\\.|[^"\\])*"', '""', source)


def declaration_only_violation(repo_root: Path, path: str) -> str | None:
    code = strip_comments_and_strings((repo_root / path).read_text())
    match = EXECUTABLE_ITEM.search(code)
    return None if match is None else f"contains `{match.group(0)}`"


@dataclass(frozen=True)
class FileCoverage:
    found: int
    hit: int

    @property
    def percent(self) -> float:
        return 100.0 if self.found == 0 else 100.0 * self.hit / self.found


def parse_lcov(text: str, repo_root: Path) -> dict[str, FileCoverage]:
    """Map repo-relative paths to line coverage. Merges repeated records."""
    lines_by_file: dict[str, dict[int, int]] = {}
    current: str | None = None
    root = repo_root.resolve()
    for raw in text.splitlines():
        line = raw.strip()
        if line.startswith("SF:"):
            path = Path(line[3:])
            if path.is_absolute():
                try:
                    path = path.resolve().relative_to(root)
                except ValueError:
                    current = None
                    continue
            current = path.as_posix()
            lines_by_file.setdefault(current, {})
        elif line.startswith("DA:") and current is not None:
            number, count = line[3:].split(",")[:2]
            hits = lines_by_file[current]
            hits[int(number)] = max(hits.get(int(number), 0), int(count))
        elif line == "end_of_record":
            current = None
    return {
        path: FileCoverage(found=len(hits), hit=sum(1 for c in hits.values() if c > 0))
        for path, hits in lines_by_file.items()
    }


def is_gated(path: str) -> bool:
    return GATED_RE.fullmatch(path) is not None


def evaluate(
    coverage: dict[str, FileCoverage],
    paths: list[str],
    repo_root: Path,
    declaration_only: dict[str, str] | None = None,
) -> tuple[bool, list[str]]:
    report: list[str] = []
    ok = True
    declared = declaration_only if declaration_only is not None else load_declaration_only(repo_root)
    gated = sorted({p for p in paths if is_gated(p) and (repo_root / p).is_file()})
    skipped = sorted({p for p in paths if not is_gated(p)})
    deleted = sorted({p for p in paths if is_gated(p) and not (repo_root / p).is_file()})
    total_found = total_hit = 0
    for path in gated:
        cov = coverage.get(path)
        if path in declared:
            problem = declaration_only_violation(repo_root, path)
            if cov is not None and cov.found > 0:
                problem = f"has {cov.found} instrumented lines"
            if problem:
                ok = False
                report.append(f"FAIL {path}: listed as declaration-only but {problem}")
            else:
                report.append(f"DECL {path}: declaration-only ({declared[path]})")
            continue
        if cov is None:
            ok = False
            report.append(f"FAIL {path}: missing coverage data")
            continue
        total_found += cov.found
        total_hit += cov.hit
        verdict = "PASS" if cov.percent >= THRESHOLD else "FAIL"
        ok = ok and verdict == "PASS"
        report.append(f"{verdict} {path}: {cov.percent:.1f}% ({cov.hit}/{cov.found} lines)")
    for path in deleted:
        report.append(f"GONE {path}: deleted, nothing to score")
    for path in skipped:
        report.append(f"OUT-OF-SCOPE {path}")
    if not gated:
        report.append("SKIP no changed executable source under src/")
    elif total_found:
        report.append(f"INFO aggregate {100.0 * total_hit / total_found:.1f}% (not a gate)")
    report.append(f"RESULT {'PASS' if ok else 'FAIL'} threshold={THRESHOLD:.0f}% per file")
    return ok, report


def self_test() -> int:
    with tempfile.TemporaryDirectory() as tmp:
        root = Path(tmp)
        src = root / "crates" / "sample" / "src"
        src.mkdir(parents=True)
        for name in ("good.rs", "thin.rs", "absent.rs", "fake_decl.rs"):
            (src / name).write_text("fn f() {}\n")
        (src / "decl.rs").write_text(
            "//! fn in a comment\npub mod a;\npub use a::b;\n#[derive(Debug)]\npub enum E { A }\n"
        )
        declared = {"crates/sample/src/decl.rs": "module facade", "crates/sample/src/fake_decl.rs": "claims no code"}
        lcov = (
            f"SF:{src / 'good.rs'}\nDA:1,1\nDA:2,1\nDA:3,1\nDA:4,1\nDA:5,0\nend_of_record\n"
            "SF:crates/sample/src/thin.rs\nDA:1,1\nDA:2,0\nDA:3,0\nend_of_record\n"
        )
        cov = parse_lcov(lcov, root)
        checks = [
            ("compliant file passes", ["crates/sample/src/good.rs"], True),
            ("C1 thin file fails", ["crates/sample/src/good.rs", "crates/sample/src/thin.rs"], False),
            ("C2 missing data fails", ["crates/sample/src/good.rs", "crates/sample/src/absent.rs"], False),
            ("non-source change is out of scope", ["README.md"], True),
            ("C3 declaration-only file passes", ["crates/sample/src/good.rs", "crates/sample/src/decl.rs"], True),
            ("C3 listed file with code fails", ["crates/sample/src/good.rs", "crates/sample/src/fake_decl.rs"], False),
        ]
        failed = 0
        for name, paths, expected in checks:
            ok, _ = evaluate(cov, paths, root, declared)
            status = "ok" if ok == expected else "BROKEN"
            failed += status == "BROKEN"
            print(f"self-test {status}: {name}")
        if cov.get("crates/sample/src/good.rs") != FileCoverage(found=5, hit=4):
            failed += 1
            print("self-test BROKEN: absolute SF path not normalised")
        return 1 if failed else 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--self-test", action="store_true")
    parser.add_argument("--lcov", type=Path)
    parser.add_argument("--changed-files", type=Path, help="one repo-relative path per line")
    parser.add_argument("--repo-root", type=Path, default=Path.cwd())
    args = parser.parse_args()
    if args.self_test:
        return self_test()
    if args.lcov is None or args.changed_files is None:
        parser.error("--lcov and --changed-files are required")
    if not args.lcov.is_file() or args.lcov.stat().st_size == 0:
        print(f"FAIL LCOV report {args.lcov} is missing or empty")
        return 1
    coverage = parse_lcov(args.lcov.read_text(), args.repo_root)
    if not coverage:
        print("FAIL LCOV report has no source records")
        return 1
    paths = [p.strip() for p in args.changed_files.read_text().splitlines() if p.strip()]
    ok, report = evaluate(coverage, paths, args.repo_root)
    print("\n".join(report))
    return 0 if ok else 1


if __name__ == "__main__":
    sys.exit(main())
