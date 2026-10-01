# Source license notices

Every first-party source file carries the Apache 2.0 notice. The repository
`LICENSE` and each package's `LICENSE` state the outbound license; the per-file
notice keeps that statement attached when a file is copied out on its own.
Dependency licenses are a separate check (`cargo deny`, in the security gate).

## The notice

A comment block opens the file, after the shebang when there is one, and holds
these two lines in this order:

```text
Copyright <year> PackDb Contributors
Licensed under the Apache License, Version 2.0
```

`PackDB` is accepted for `PackDb`, and a year range such as `2025-2026` for a
single year. The year is the year the file was created; existing statements are
not rewritten. The full Apache boilerplate that continues after the second line
in some Rust files is also accepted. Rust uses `//` comments, before any `//!`
crate or module documentation; Python and shell use `#`. A shebang stays on the
first line so the script remains executable.

An SPDX identifier is not an accepted form. Adopting one requires a policy
change approved by the maintainers and applied to all four repositories at once.

## Scope

`scripts/check_source_headers.py` examines every file tracked by git:

- **Source**, which must carry the notice: `.rs`, `.py`, `.sh` and `.bash`
  files, and extensionless scripts whose shebang runs `python`, `python3`,
  `bash` or `sh`. This includes examples, tests and scripts.
- **Declared non-source**: `.md`, `.toml`, `.lock`, `.yml`, `.yaml`, `.json`,
  `.jsonl`, `.txt` and `.sha256` files, and files named `LICENSE`, `CODEOWNERS`
  and `.gitignore`.

Any other file fails as unclassified. Adding a file of a new type means
deciding which list it belongs on and changing the checker as described below;
a new source language also needs its comment marker.

## Exceptions

A source file that must not carry this notice -- generated output, or
third-party code with its own notice -- is listed by exact path in
`scripts/source_header_exceptions.txt`:

```text
path<TAB>provenance<TAB>reason
```

Provenance names where the file came from (upstream project and version, or the
generator and its input); the reason says why the notice does not apply. Keep
third-party notices as they are and do not relabel third-party code. An
exception fails as stale once its file is untracked, is not source, or carries
the notice.

## Running it

```sh
python3 scripts/check_source_headers.py              # self-test, then the repository
python3 scripts/check_source_headers.py --self-test  # self-test only
```

`scripts/oss-ci.sh checks` runs it, so the required `oss-ci / checks` job and
the candidate release validation (which runs `checks` in full) both fail on a
missing or wrong notice. The self-test runs before every check; it covers the
accepted forms, a missing notice, a wrong license, a displaced shebang, invalid
and stale exceptions, and unclassified file types.

## Updating the checker

packdb-sdk, packdb-cli, logql-parser and traceql-parser carry byte-identical
copies of `scripts/check_source_headers.py` and of this document. To change
either:

1. Make the change in one repository, add a self-test control for it, and bump
   `CHECKER_VERSION` when the accepted forms or the scope change.
2. Copy both files unchanged into the other three repositories and open one PR
   in each. Each PR's `checks` job must pass against that repository's files.
3. Confirm the copies match before merging the last PR:
   `shasum -a 256 scripts/check_source_headers.py docs/SOURCE_HEADERS.md`
   prints the same digests in all four repositories.

Repository-specific content belongs only in `scripts/source_header_exceptions.txt`.
