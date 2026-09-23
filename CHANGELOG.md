# Changelog

## Unreleased

- Prepared `packdb-query-cli` as a standalone Apache-2.0 workspace.
- Published the binary compatibility contract with the crate, covering flags,
  output formats, transport selection, exit codes and the release matrix.
- Release candidates build two native targets, `x86_64-unknown-linux-gnu` and
  `aarch64-apple-darwin`, with `flight-sql` enabled and the Linux binary held to
  a declared glibc 2.35 floor.
- Added reproducible export provenance and private candidate workflows.
