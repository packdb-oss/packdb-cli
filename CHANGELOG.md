# Changelog

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
