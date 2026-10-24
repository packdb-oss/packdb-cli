# Changelog

## Unreleased

Breaking. `packdb-query` is retired in favour of `packdb`, whose command library
this repository now provides; its 0.2.x archives are unchanged. See
`crates/packdb-cli/COMPATIBILITY.md` for the contract and the migration table.

- The package is `packdb-cli`, a library with no executable. `packdb query sql`
  replaces `packdb-query`; the `packdb` binary is assembled with the server.
- Credentials never come from argument values: `--api-key-file`, `--token-file`
  and `--basic-auth-file`, or `PACKDB_API_KEY`, `PACKDB_TOKEN`, `PACKDB_BASIC_AUTH`
  and their `_FILE` forms. Two credentials are refused instead of the first
  winning silently.
- `--endpoint` / `PACKDB_ENDPOINT` replace `--url` / `PACKDB_URL`;
  `PACKDB_FLIGHT_ENDPOINT` replaces `PACKDB_FLIGHT_URL`; `--transport
  auto|http|flight-sql` replaces `--http-only`, and an explicit transport never
  falls back.
- Results report completeness. JSON output is one envelope with `completeness`,
  `partial_causes`, `warnings`, `transport` and `data`; table and CSV output warn
  on stderr. An answer whose completeness is partial or unknown exits `3`.
- Exit codes: `4` not authenticated, `5` permission denied, `6` unreachable, `7`
  server or protocol failure, `8` output could not be delivered (writing or
  flushing stdout or stderr failed).
- `packdb query promql`, `packdb query logql`, `packdb query traceql` and
  `packdb trace <id>` use the same credentials, output envelope, completeness and
  exit codes. Times are RFC 3339, Unix seconds, `now` or `now-<duration>`; windows
  default to the last hour. Their completeness is `unknown` unless the server
  reports causes, except a found trace, which is `complete` without them. A trace the
  server reports it does not have exits `1`; any other `404` exits `7`.
- Table and CSV columns of `query sql` keep the order in which the response lists
  them instead of being sorted by name. That order is what the client receives;
  it is not promised to match the `SELECT` list.
- Requires `packdb-client` with the transport and error changes of
  packdb-oss/packdb-sdk#34, and with the query API client (`QueryApiClient`) of
  packdb-oss/packdb-sdk#36.

## 0.2.4

- Build against SDK 0.2.4. No changes to the CLI's flags, output or exit codes.
- Replace the LICENSE files (repository root and `packdb-query-cli`, also
  copied into every binary archive) with the verbatim Apache License 2.0 text.
  Earlier releases shipped an altered text: the Trademarks clause (section 6)
  was missing, sections 7 to 9 were renumbered 6 to 8, the copyright grant
  wording differed, and section 9 was retitled. The CLI's licensing
  (Apache-2.0, as declared in `Cargo.toml` and the source headers) is
  unchanged. Earlier release archives keep their original text; this release
  supersedes them.
- Update the locked `rustls-webpki` to 0.103.16 and `webpki-root-certs` to
  1.0.10, resolving RUSTSEC-2026-0382 and RUSTSEC-2026-0383. The release
  binaries are built from this lockfile. `--locked` reproduces the reviewed
  dependency versions (`cargo build --locked`, `cargo install --locked`).
  Unlocked builds may also resolve patched versions; verify the resolved
  versions and audit the resulting lockfile.

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
