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
- The default endpoint is `http://127.0.0.1:8080`, where the PackDB server's HTTP
  listener binds by default; `packdb-query` defaulted to `http://localhost:4000`.
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
- Server `warnings` and `infos` from PromQL and LogQL answers are printed on stderr
  (`warning: server: `, `note: server: `) and carried in the JSON envelope's new
  `server_warnings` and `server_infos` lists, empty for `query sql`.
- An answer that is not the shape its API defines exits `7` in every format instead
  of printing empty rows or blank cells.
- `packdb doctor` runs limited diagnostics of a running server without creating
  state on it: it reads `GET /info` and the server's read-only
  `GET /health/diagnostics` for health, readiness (of the server's own monitoring
  scope) and registered routes. OTLP route and query surface checks are `withheld`,
  because only routes that can create tenant state would show them working, so the
  report stays incomplete and doctor exits `3`; checks the server's HTTP API does not
  expose are listed as `not_exposed`. It prints a text report or the JSON envelope
  with a `packdb.cli.diagnostics.v1` report.
- `packdb smoke-test --allow-write` writes a small OTLP/JSON fixture marked with a
  run-unique identifier and reads it back on PromQL, LogQL, TraceQL, trace lookup,
  Jaeger and SQL until a visibility timeout, filtering every read on this run. Writes
  are never retried, and an acknowledgement that does not decode as an OTLP export
  response is never counted as zero rejected records.
- `packdb support-bundle` writes a deterministic `.tar.zst` with the doctor report,
  the server's `/info` and read-only `/health/diagnostics`, and a description of the
  client, plus a manifest and SHA-256 checksums. Every file is scanned for secrets
  and for the configured credential first; a match leaves the file out, and an
  existing output file is never replaced.
- The support bundle is compressed with `zstd` 0.14.3; 0.13.3 and the `zstd-safe`
  and `zstd-sys` versions it used carry RustSec advisories.
- `packdb usage-report`, an operator command, reports the server's resource usage
  from its read-only `GET /health/usage`: tier memory (process-wide, current),
  points accepted since the process started (process-wide), and the server's own
  monitoring tenant's storage (current), each with its scope and period. It is not
  a report of the caller's own tenant. A value the server reports unavailable leaves
  the report incomplete (exit `3`) instead of reading as zero; ingest per signal or
  tenant, windowed totals, query counts and limits are listed as `not_exposed`.
- Table and CSV columns of `query sql` keep the order in which the response lists
  them instead of being sorted by name. That order is what the client receives;
  it is not promised to match the `SELECT` list.
- `query sql` table and CSV output no longer change values. Every number was
  printed as a 64-bit float, so integers above 2^53 lost digits (a span ID
  `1791227706083264631` printed as `1.7912277060832645e18`) and `1` printed as
  `1.0`. Each column also took its type from the first row, so a later value of
  another type, or any value in a column that started null, printed empty. Each
  cell is now the decoded value; JSON output was already exact.
- Requires `packdb-client` with the transport and error changes of
  packdb-oss/packdb-sdk#34, with the query API client (`QueryApiClient`) of
  packdb-oss/packdb-sdk#36, without `tonic`'s `zstd` feature
  (packdb-oss/packdb-sdk#37), and with exact table and CSV cells
  (packdb-oss/packdb-sdk#42).

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
