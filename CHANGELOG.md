# Changelog

## 0.2.3

- Build against SDK 0.2.3, which redacts credentials from diagnostic formatting.
- Update the locked Rustls dependency to 0.23.46, resolving
  RUSTSEC-2026-0346, RUSTSEC-2026-0347, RUSTSEC-2026-0348 and RUSTSEC-2026-0349.
- Include dependency license texts and attribution in every binary archive.
- Support single-maintainer public operation with recorded review and passing
  checks. Pin candidate qualification to Ubuntu 24.04 for Python compatibility.

## 0.2.2

- Built against `packdb-client` 0.2.2. The CLI's flags, output and exit codes
  are unchanged.
- Replace the yanked `yoke-derive 0.8.3` lockfile entry with `0.8.4`.
- Add Dependabot and a scheduled action-pin check, require the Apache 2.0
  notice in every source file, and record this repository as the canonical
  source of the CLI.

## 0.2.1

- Invalid SQL now exits `1` (SQL error) as `COMPATIBILITY.md` documents; 0.2.0
  exited `3`. Rejected credentials (`401`/`403`) exit `2`. Other non-2xx server
  responses still exit `3`. Built against `packdb-client` 0.2.1.

## 0.2.0

- Prepared `packdb-query-cli` as a standalone Apache-2.0 workspace.
- Published the binary compatibility contract with the crate, covering flags,
  output formats, transport selection, exit codes and the release matrix.
- Release candidates build two native targets, `x86_64-unknown-linux-gnu` and
  `aarch64-apple-darwin`, with `flight-sql` enabled and the Linux binary held to
  a declared glibc 2.35 floor.
- Added reproducible export provenance and private candidate workflows.
