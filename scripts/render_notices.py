#!/usr/bin/env python3
# Copyright 2026 PackDB Contributors
# Licensed under the Apache License, Version 2.0
"""Render cargo-about JSON and preserve original dependency legal files."""

import argparse
import json
from pathlib import Path
import tempfile


def render(data):
    """Fail on incomplete license data; never include machine-local paths."""
    crates = data.get("crates", [])
    licenses = data.get("licenses", [])
    if not crates or not licenses:
        raise ValueError("empty dependency/license inventory")
    expected = {(x["package"]["name"], x["package"]["version"]) for x in crates}
    covered = set()
    sections = [
        "Third-party licenses and notices for packdb-query",
        "Generated from the locked shipping dependency graph using cargo-about 0.9.2.",
        "Build dependencies are included because generated code may be linked.",
        "The project license is also included separately as LICENSE.",
    ]
    for license in licenses:
        text = license.get("text", "")
        users = license.get("used_by", [])
        if not text.strip() or not users:
            raise ValueError("license has no text or dependent crates")
        names = sorted({(x["crate"]["name"], x["crate"]["version"]) for x in users})
        covered.update(names)
        sections += [
            "=" * 72,
            license["name"] + " (" + license["id"] + ")",
            "Used by: " + ", ".join(n + " " + v for n, v in names),
            text,
        ]
    if not expected <= covered:
        raise ValueError("dependency missing license text: " + repr(sorted(expected - covered)))

    # SPDX templates may omit package-specific copyright holders or notices.
    # Preserve the original legal files as well, including vendored crypto notices.
    for crate in sorted(crates, key=lambda x: (x["package"]["name"], x["package"]["version"])):
        package = crate["package"]
        base = Path(package["manifest_path"]).resolve().parent
        if not base.is_dir():
            raise ValueError("dependency source unavailable: " + package["name"])
        legal = set()
        for path in base.rglob("*"):
            if path.is_file() and path.name.upper().startswith(
                ("LICENSE", "LICENCE", "COPYING", "NOTICE", "COPYRIGHT", "AUTHORS")
            ):
                legal.add(path)
        if package.get("license_file"):
            legal.add(base / package["license_file"])
        for path in sorted(legal):
            resolved = path.resolve()
            if not resolved.is_relative_to(base) or not resolved.is_file():
                raise ValueError("invalid dependency legal file: " + package["name"])
            text = resolved.read_text(encoding="utf-8")
            if not text.strip():
                continue
            sections += [
                "=" * 72,
                package["name"] + " " + package["version"] + ": " + path.relative_to(base).as_posix(),
                text,
            ]
    return "\n\n".join(sections) + "\n"


def self_test():
    with tempfile.TemporaryDirectory() as tmp:
        base = Path(tmp)
        (base / "Cargo.toml").write_text("[package]\nname = 'fixture'\n")
        (base / "NOTICE").write_text("Copyright Fixture Contributor\nPreserve this attribution.\n")
        (base / "vendor").mkdir()
        (base / "vendor/LICENSE").write_text("Vendored copyright and license terms.\n")
        package = {"name": "fixture", "version": "1.0.0", "manifest_path": str(base / "Cargo.toml")}
        data = {
            "crates": [{"package": package}],
            "licenses": [{"name": "Fixture license", "id": "MIT", "text": "Fixture license terms.",
                          "used_by": [{"crate": package}]}],
        }
        output = render(data)
        assert "Fixture license terms." in output
        assert "Copyright Fixture Contributor" in output
        assert "Vendored copyright and license terms." in output
        assert str(base) not in output
        for bad in [
            {"crates": [], "licenses": []},
            {"crates": data["crates"], "licenses": []},
            {"crates": [{"package": dict(package, name="missing")}], "licenses": data["licenses"]},
            {"crates": data["crates"], "licenses": [dict(data["licenses"][0], text="")]},
            {"crates": [{"package": dict(package, license_file="../outside")}], "licenses": data["licenses"]},
        ]:
            try:
                render(bad)
            except ValueError:
                pass
            else:
                raise AssertionError("invalid notice inventory accepted")
    print("notice renderer: attribution, vendored notices and negative controls passed")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--input", type=Path)
    parser.add_argument("--output", type=Path)
    parser.add_argument("--self-test", action="store_true")
    args = parser.parse_args()
    if args.self_test:
        self_test()
        return
    if args.input is None or args.output is None:
        parser.error("--input and --output are required")
    args.output.write_text(render(json.loads(args.input.read_text())), encoding="utf-8")


if __name__ == "__main__":
    main()
