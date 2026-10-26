# `packdb` command contract

What the `packdb` client commands promise their callers. This library implements
them; the `packdb` binary that embeds it is assembled with the server. Until that
binary exists, the contract is tested through `packdb-cli-test-harness`, a
test-only executable that is never released.

Read as: anything in **The contract** changes only with a deliberate, documented
decision. Anything under **Not promised** may change without one.

This contract replaces the `packdb-query` contract of releases 0.2.0 to 0.2.4.
`packdb-query` is retired; its released archives are unchanged. See
*Migrating from `packdb-query`* below.

## The contract

### Commands

```
packdb query sql <SQL> [connection] [--transport auto|http|flight-sql] [--format table|json|csv]
packdb query promql <EXPR> [connection] [--time T | --start T --end T --step D] [--format table|json|csv]
packdb query logql <EXPR> [connection] [--time T | --start T --end T --step D] [--limit N] [--direction forward|backward] [--format table|json|csv]
packdb query traceql <QUERY> [connection] [--start T] [--end T] [--limit N] [--format table|json|csv]
packdb trace <TRACE_ID> [connection] [--start T --end T] [--format json]
packdb doctor [connection] [--format text|json]
packdb smoke-test --allow-write [connection] [--visibility-timeout SECS] [--format text|json]
packdb support-bundle [connection] [--output PATH] [--format text|json]
packdb usage-report [connection] [--format text|json]
```

`packdb` with no subcommand prints help to stderr and exits `2`; it never starts a
server. `--help` and `--version` exit `0`.

### Connection and credentials

| flag | environment | default |
| --- | --- | --- |
| `--endpoint URL` | `PACKDB_ENDPOINT` | `http://127.0.0.1:8080`, the server's default HTTP listener |
| `--flight-endpoint URL` | `PACKDB_FLIGHT_ENDPOINT` | — |
| `--api-key-file PATH` | `PACKDB_API_KEY` or `PACKDB_API_KEY_FILE` | — |
| `--token-file PATH` | `PACKDB_TOKEN` or `PACKDB_TOKEN_FILE` | — |
| `--basic-auth-file PATH` | `PACKDB_BASIC_AUTH` or `PACKDB_BASIC_AUTH_FILE` | — |
| `--tenant ID` | `PACKDB_TENANT` | — |
| `--timeout SECS` | `PACKDB_TIMEOUT` | `30` |

- **Secrets never come from arguments.** Credential flags name a file; environment
  variables carry the value or, with `_FILE`, a path. A secret passed as an argument
  value is a usage error.
- **Flags win over the environment.** The highest level that supplies any credential
  is used in full; credentials at a lower level are ignored.
- **Conflicts are refused.** Two credential kinds at the deciding level, or a
  variable and its `_FILE` form together, exit `2` naming both sources. Nothing is
  sent.
- A credential file must be valid UTF-8, at most 64 KiB, and non-empty once trailing
  whitespace is removed. Basic authentication holds `user:password`.
- The API key is sent as `X-API-Key`, the token as `Authorization: Bearer`, and the
  tenant as `X-Scope-OrgID`, only when a tenant is given.
- Secrets never appear on stdout or stderr. User information in an endpoint URL is
  removed before the endpoint is printed.

### Times and windows

- A time `T` is RFC 3339 in UTC (`2026-10-23T12:00:00Z`), Unix seconds with up to
  nine fractional digits (`1761220800.25`), `now`, or `now-<duration>` (`now-15m`).
  A duration `D` is written like `15s`, `1m` or `1h30m` and is at least `1ms`.
- Each API receives its own form without rounding: Unix seconds for PromQL,
  nanoseconds for LogQL, and whole seconds for TraceQL and `trace`, where the window
  is widened (start rounded down, end rounded up).
- A window's `--end` defaults to now and its `--start` to one hour before `--end`. A
  start after the end exits `2`.
- `query promql` evaluates at one instant (`--time`, or the server's current time)
  unless `--start`, `--end` or `--step` is given; then it queries the range, with a
  default step of the window over 250 points, at least `1s`.
- `query logql` queries the window by default; `--step`, `--limit` and
  `--direction` default to the server's. `--time` evaluates a metric expression at
  one instant instead.
- `query traceql` searches the window. `trace` searches the whole store unless
  `--start` and `--end` are both given.
- `--time` cannot be combined with `--start`, `--end` or `--step`: exit `2`.

### Transport (`query sql`)

- `--transport http` and `--transport flight-sql` never fall back.
- `--transport auto`, the default, uses Flight SQL when a Flight endpoint is
  configured, otherwise HTTP. It falls back to HTTP once, only when the Flight SQL
  connection could not be opened before the query was sent: refused, host or network
  unreachable, or a timed-out connect. Authentication, permission, TLS and query
  errors, and any failure after the query may have been sent, are reported, not
  retried.
- A fallback is reported, as a warning on stderr and in JSON `transport`, whether
  the query then succeeds or fails.
- `flight-sql` needs a Flight endpoint and a build with the `flight-sql` feature;
  otherwise it exits `2`.
- The other commands use HTTP only. JSON `transport` reports `requested` and `used`
  as `http`, and `used` as `null` when nothing was sent.

### Output

- **stdout carries the result and nothing else.** Warnings, notes and errors go to
  stderr, prefixed `warning: `, `note: ` or `error: `.
- `--format table` (default) and `--format csv` print the rows. `--format json`
  prints one JSON object:

  ```json
  {
    "schema_version": "packdb.cli.v1",
    "command": "query sql",
    "endpoint": "http://127.0.0.1:8080",
    "transport": {"requested": "auto", "used": "http", "fallback_reason": null},
    "completeness": "unknown",
    "partial_causes": [],
    "warnings": ["completeness unknown (...)"],
    "server_warnings": [],
    "server_infos": [],
    "error": null,
    "data": {"rows": [{"connected": 1}]}
  }
  ```

  On a failure after the request is sent, `error` holds `kind` and `message`,
  `data` and `completeness` are `null`, `transport` names the transport that failed
  and any fallback, and the message is also on stderr.
- `data.rows` holds the rows as `packdb-client` returns them: decoded from the HTTP
  JSON response, or converted from Flight SQL record batches. It is not the raw wire
  payload.
- For `query promql`, `query logql`, `query traceql` and `trace`, `data` is the
  server's decoded JSON response body: the Prometheus or Loki `{status, data}`
  object, the Tempo search response, or the trace.
- Table and CSV columns, every cell as text exactly as the server sent it:

  | command | columns |
  | --- | --- |
  | `query promql`, `query logql` metric results | `labels`, `timestamp`, `value` |
  | `query logql` log lines | `labels`, `timestamp`, `line`, `metadata` |
  | `query traceql` | `traceID`, `rootServiceName`, `rootTraceName`, `startTimeUnixNano`, `durationMs` |

  `labels` is written `{name="value", ...}` with names sorted. A scalar or string
  result is one row with `labels` `{}`. `trace` has no table form: `--format table`
  or `csv` exits `2` and nothing is sent.
- `error.kind` is one of `query_rejected`, `not_found`, `unauthenticated`,
  `permission_denied`, `unreachable`, `configuration` or `server_or_protocol`.
- `warnings` are the command's own. The `warnings` and `infos` lists a Prometheus or
  Loki answer carries are reported as received: in `server_warnings` and
  `server_infos`, and on stderr as `warning: server: ...` and `note: server: ...`.
  They do not change the exit code. Both lists are empty for `query sql`.
- An answer that is not the shape its API defines is a server or protocol failure
  (`7`, `error.kind` `server_or_protocol`) in every format, and nothing is printed on
  stdout except the JSON envelope. That covers a `status` other than `success`, a
  missing or mistyped field, a sample or log entry of the wrong arity, an unknown
  result type, a search result missing any of the five columns, a trace without a
  `trace` object, and `warnings` or `infos` that are not lists of text. A result is
  never rendered with empty rows or blank cells in place of what is missing.

### Completeness

`completeness` is one of:

- `partial`: the server reported causes (`x-packdb-partial-causes`, or Flight SQL
  initial metadata). They are listed in `partial_causes`.
- `complete`: only where the protocol's documented and verified contract says an
  absent cause list means complete, and none was reported.
- `unknown`: everything else. Unknown is never reported as complete.

For `query sql`, `query promql`, `query logql` and `query traceql`, an absent cause
list is **`unknown`**: those server paths have not been verified to mark every
incomplete result, and a trace search stops at its limits without marking the
result. For `trace`, an absent cause list on a found trace is **`complete`**: a
lookup the server could not finish answers with causes, and a trace found nowhere is
a `404`. In table and CSV output, a
`partial` or `unknown` result prints `warning: incomplete result (...)` or
`warning: completeness unknown (...)` on stderr. Truncation by your own `LIMIT` is
not reported by any cause.

### Exit codes

| code | meaning |
| --- | --- |
| `0` | succeeded with a complete result |
| `1` | the server rejected the query |
| `2` | invalid usage or local configuration; nothing was sent |
| `3` | succeeded, but completeness is partial or unknown |
| `4` | not authenticated (`401`, gRPC `Unauthenticated`) |
| `5` | permission denied (`403`, gRPC `PermissionDenied`) |
| `6` | the server could not be reached |
| `7` | server or protocol failure (any other non-2xx, including every `5xx`, or an answer that is not the shape its API defines) |
| `8` | output could not be delivered: writing or flushing stdout or stderr failed (a full disk, a closed pipe), including warnings and error messages |

When several apply, the first match in the order `8, 2, 4, 5, 6, 7, 1, 3` wins. A
result that was not fully written never exits `0` or `3`. Over
HTTP a `400` whose body is the server's query or parse rejection (`Query error: ...`
or `Parse error: ...`) is `1`; over Flight SQL, `InvalidArgument` is `1`. For
`query promql`, `query logql` and `query traceql` any `400` is `1`, and so is a
TraceQL search `422` (a query the server does not support). A `trace` the server
reports it has no part of (a `404` with the body `{"error": "trace not found: <id>"}`
for the requested ID and no partial causes) is `1` with `error.kind` `not_found`; any
other `404`, such as a missing route or a proxy's page, is `7`.

### `doctor`

`packdb doctor` runs limited diagnostics of a running server and prints a report: one
line per check with `--format text` (the default), or the JSON envelope with the
report under `data` with `--format json`.

Doctor is meant to create no state on the server it examines. The server's
`/health/detailed`, `/ready`, OTLP and query routes resolve the request's tenant, and
several create the tenant's store on first use, so doctor does not call them. It sends
`GET /info` and `GET /health/diagnostics`, the server's read-only health and
readiness. In the open-source PackDB server neither handler creates tenant state; a
build that adds middleware in front of them is not covered by that statement.

Health and readiness from `/health/diagnostics` describe the server's own monitoring
scope, not the tenant in the request. Its route list says which OTLP and query routes
are registered; it does not show that ingestion or queries work, so the checks that
would (`ingest.otlp_routes_mounted`, `query.surface_availability`) stay `withheld`
until the server offers a way to prove them that creates nothing. The report is
therefore always `incomplete`, and doctor exits `3` when nothing else applies. A
server without `/health/diagnostics` (`404`) leaves health and readiness `withheld`
too.

Each check is one of `pass`, `warn`, `fail`, `error` (its evidence could not be
collected), `withheld` (a required check deliberately not run) or `not_exposed` (the
server's HTTP API gives no evidence for it). The last three are verdicts of unknown;
none is ever reported as a pass. `error` and `withheld` checks are listed under
`missing_evidence`.

| check | outcome |
| --- | --- |
| `process.version` | `GET /info`: the server's name and version, with the environment it reports |
| `process.health` (required) | `GET /health/diagnostics`: `healthy` passes, `degraded` warns, `unhealthy` fails; names the checks that are not healthy |
| `process.ready` (required) | `GET /health/diagnostics`: ready passes, not ready fails; names the checks that are not ready |
| `routes.registered` | `GET /health/diagnostics`: the OTLP and query routes the server reports as registered |
| `ingest.otlp_routes_mounted`, `query.surface_availability` (required) | `withheld` |
| `process.build_match`, `durability.ages`, `durability.recovery_state`, `storage.tiers_attached`, `ingest.rejection_ratio`, `ingest.backpressure`, `limits.query_pool`, `limits.admission`, `limits.cardinality` | `not_exposed` |

- An answer counts as evidence only when it is a complete, unredirected `200` of at
  most 8 MiB in the route's documented shape: `/info` with the service's `name` and
  `version`; `/health/diagnostics` with `schema_version`
  `packdb.server.diagnostics.v1`, `health`, `readiness` and `routes`. Anything else is `error` and listed
  under `missing_evidence` with its reason, also printed on stderr as
  `note: missing ...`.
- One deadline, `--timeout` from the first attempt, covers every attempt, wait and
  body of each request; each attempt gets only the time that remains. A `429` is
  retried at most twice, after `Retry-After` seconds or 250 ms when none is given,
  and only when that wait is at most 2 s and ends before the deadline. A longer wait,
  or an HTTP-date `Retry-After`, is reported as a throttle without retrying. Nothing
  else is retried.
- Exit: `1` when a required check fails; `3` while any required check is withheld or
  its evidence is missing; `4` or `5` when the server refuses the credential; `6`
  when the server sends no response to any of doctor's requests. Once it has sent
  any response, even a refusal, throttle or redirect, a later request it does not
  answer is missing evidence (`3`), and the envelope's `transport.used` stays
  `http`. The precedence above applies. The envelope's
  `completeness` is `unknown`; the report's is `incomplete`. stderr always carries
  `warning: completeness unknown (limited diagnostics: ...)`.
- The report (`schema_version` `packdb.cli.diagnostics.v1`) holds `report_kind`,
  `started_at`, `finished_at`, `tool`, `target` (the endpoint without user
  information, whether a tenant was supplied, and the server's environment),
  `requested_scope` (`diagnostics`: `limited`), `completeness`, `counts`, `findings`
  (`id`, `status`, `severity` `required` or `info`, `summary`, `evidence`) and
  `missing_evidence` (`evidence`, `reason`).

### `smoke-test`

`packdb smoke-test` writes a small fixture to the server and reads this run's data back
on every query surface. It writes only with `--allow-write`; without it, it exits `2`
and sends nothing. The fixture goes to the tenant the connection names, or to the
server's default tenant when none is given.

- **Fixture** (`packdb.smoke-fixture.v1`), sent as OTLP/JSON to `/v1/metrics`,
  `/v1/logs` and `/v1/traces`: a counter (7), a gauge (3), an integer gauge (128) and a
  histogram (4 observations); two log records; a trace of three spans across two
  services, one of them an error. Every resource, record and span carries
  `packdb.smoke.run_id`, a marker generated for this run, and the trace ID is new for
  this run too. The text report names the run; stderr notes that it was written.
- **Writes** are sent once each and never retried, not even after a `429`. A write
  passes only when the server answers `2xx` with an acknowledgement that rejects
  nothing: an empty body, or an `application/x-protobuf` OTLP export response with no
  rejected records. Rejected records fail the write; any other acknowledgement —
  undecodable, carrying fields the export response does not define, or another
  content type — is missing evidence, never zero rejections. A refused credential or
  an unreachable server stops the remaining writes. A signal whose write did not pass
  is not read.
- **Reads** filter on this run's marker or trace ID, so another run's data cannot
  satisfy them, and repeat every second until each matches exactly or
  `--visibility-timeout` (default 60, at most 300 seconds) passes:

  | check | request | expected |
  | --- | --- | --- |
  | `read.promql.counter`, `.gauge`, `.integer`, `.histogram_count` | `sum(<metric>{packdb_smoke_run_id="<run>"})` at the run's end | 7, 3, 128, 4 |
  | `read.logql.records` | `` {service_name="packdb-smoke-frontend"} \| packdb_smoke_run_id = `<run>` `` over the run's window | 2 records |
  | `read.traceql.search` | `{ span.packdb.smoke.run_id = "<run>" }` over the run's window | 1 trace with the run's trace ID |
  | `read.trace.spans` | `GET /api/v2/traces/<trace>` | 3 spans |
  | `read.jaeger.spans` | `GET /jaeger/api/traces/<trace>` | 3 spans |
  | `read.sql.error_span` | `SELECT name FROM traces.packdb_smoke_backend WHERE status_code = 2 AND trace_id = '<trace>'` | `backend.write` |
  | `read.sql.service_spans` | `SELECT span_id FROM traces.<service> WHERE trace_id = '<trace>'` for both services | 1 + 2 spans |

  Each read gets only the time left before the timeout. A read still not matching at
  the timeout fails; a read that never got a usable answer (a partial result counts as
  not yet visible), or was not answered before the timeout, is missing evidence. A
  read refused with `401` or `403`, or a server that stops answering, is recorded at
  once rather than retried.
- Exit: `1` when a write or read fails; `3` when evidence is missing; `4` or `5` when
  the server refuses the credential; `6` when the server sends no response to any
  write; otherwise `0`. The report (`packdb.cli.diagnostics.v1`, `report_kind`
  `smoke_test`) records the run marker, trace ID, fixture version and visibility
  timeout under `requested_scope`.
### `support-bundle`

`packdb support-bundle` collects diagnostics into one archive for support. It sends
what `doctor` sends — `GET /info` and `GET /health/diagnostics`, nothing that creates
tenant state — and writes `--output` (default
`packdb-support-bundle-<UTC time>.tar.zst` in the working directory).

- **Contents:** `manifest.json` (`format` `packdb.cli.support-bundle.v1`, creation
  time, tool, every file with its size and SHA-256, and the bundle report),
  `checksums.sha256` (`shasum -a 256 -c` format), `doctor/report.json`,
  `server/info.json` and `server/diagnostics.json` (the server's answers as received,
  when usable), and `client/environment.json` (tool version, platform, endpoint
  without user information, whether a tenant and a Flight endpoint are set, the
  credential kind, the timeout, and the names of the `PACKDB_*` variables that are
  set — never a value).
- **No secrets:** every file is scanned before it is written for private keys, JWTs,
  `Authorization` headers, bearer tokens, password, secret, token and key
  assignments, URL user information, AWS access key IDs, and the exact credential the
  connection uses — at any length, as written, JSON-escaped, and inside every decoded
  key and string of a JSON file. A short credential can withhold more than it needs
  to, never less. A file that matches is left out and reported `withheld`, naming the
  class found but never the text; if the manifest or checksums would match, no
  archive is written.
- **Bounds:** each server answer is read to at most 8 MiB, the files together to at
  most 32 MiB.
- **Writing:** a deterministic tar (fixed modes, owners and times) compressed with
  zstd, written to a new temporary file beside the destination (a random name,
  readable only by its owner), read back and checked against what was staged, then
  moved into place without replacing anything. Only that temporary file is ever
  removed. If `--output` exists, the command exits `2` before sending anything.
- Exit: `3` while any required file is missing or withheld or the included doctor
  report is incomplete (it is, while doctor withholds checks); `4` or `5` when the
  server refuses the credential; `6` when the server sends no response, in which case
  the client side is still bundled; `8` when the archive cannot be written. The
  report (`packdb.cli.diagnostics.v1`, `report_kind` `support_bundle`) lists one
  finding per file and `bundle.archive` with the path, size and SHA-256.

### `usage-report`

`packdb usage-report` reports a running server's resource usage: one line per
measurement with `--format text` (the default), or the JSON envelope with the report
under `data` with `--format json`. It sends one request, `GET /health/usage`, which in
the open-source PackDB server creates no tenant state and does not resolve the
request's tenant.

Each measurement keeps the scope and period the server declares, and the report
refuses an answer whose declarations differ from these:

| check | scope | period | value |
| --- | --- | --- | --- |
| `usage.memory` (required) | `process` | `current` | bytes resident in the storage tiers, their budget, and allocator memory outside them |
| `usage.ingest` (required) | `process` | `since_process_start` | points accepted across all signals and protocols; a restart resets it |
| `usage.storage` (required) | `tenant` | `current` | the server's own tenant: logical and stored bytes per attached tier, and metric names and rows in the hot tier |
| `usage.ingest_by_signal`, `usage.ingest_by_tenant`, `usage.windowed_totals`, `usage.queries`, `usage.limits` | | | `not_exposed` |

- A value the server reports as unavailable (`null` with a reason) is `error` and
  listed under `missing_evidence` with that reason, never reported as zero. A server
  without `/health/usage` (`404`) leaves the three measurements `withheld`.
- An answer counts as evidence only when it is a complete, unredirected `200` with
  `schema_version` `packdb.server.usage.v1` and every measurement in the shape above;
  anything else leaves all three `error`.
- `usage.memory`'s resident bytes exclude the cold tier when its budget is unbounded,
  so they need not equal the sum of the three tier fields.
- Hot-tier metric names and rows are counted as stored: a histogram is one name,
  where PromQL lists its `_bucket`, `_count` and `_sum` series separately. Neither
  count is comparable with `usage.ingest`, which sums points across all signals.
- Exit: `0` when all three measurements are present; `3` while any is unavailable,
  withheld or unusable; `4` or `5` when the server refuses the credential; `6` when it
  sends no response. The report is `packdb.cli.diagnostics.v1` with `report_kind`
  `usage-report`; each present measurement's `evidence` holds `scope`, `period`,
  `value` and, for storage, `tenant_id`.

## Not promised

- **Exact table layout.** Parse `json` or `csv` instead.
- **Error and warning text** after the `error: ` / `warning: ` prefix.
- **`--help` and `--version` formatting.**
- **Ordering of result rows**, unless the query orders them.
- **Column order of `query sql` table and CSV output.**
- **`doctor` and `usage-report` summaries and evidence values**, beyond the check
  identifiers, statuses, severities, scopes and periods listed above.
- **JSON key order and whitespace.**

## Migrating from `packdb-query`

| `packdb-query` (0.2.x) | `packdb` |
| --- | --- |
| `packdb-query '<SQL>'` | `packdb query sql '<SQL>'` |
| `--url` / `PACKDB_URL` | `--endpoint` / `PACKDB_ENDPOINT` |
| default `http://localhost:4000` | default `http://127.0.0.1:8080` |
| `--flight-endpoint` / `PACKDB_FLIGHT_URL` | `--flight-endpoint` / `PACKDB_FLIGHT_ENDPOINT` |
| `--api-key VALUE` | `PACKDB_API_KEY`, `PACKDB_API_KEY_FILE` or `--api-key-file` |
| `--token VALUE` | `PACKDB_TOKEN`, `PACKDB_TOKEN_FILE` or `--token-file` |
| `--basic-auth user:pass` | `PACKDB_BASIC_AUTH`, `PACKDB_BASIC_AUTH_FILE` or `--basic-auth-file` |
| `--http-only` | `--transport http` |
| first credential wins silently | two credentials are refused (exit `2`) |
| `ERROR: ` on stderr | `error: ` on stderr |
| exit `2` for an unreachable server or refused credentials | `6`, `4` or `5` |
| exit `3` for a server failure | `7` |
| exit `0` with completeness unreported | `3` with a warning unless complete |
| any Flight SQL failure fell back to HTTP | fallback only before the query was sent |

## How this is tested

- `crates/packdb-cli` unit tests: credential precedence and every conflict,
  credential files, exit precedence, completeness classification, endpoint
  redaction and the JSON envelope.
- `crates/packdb-cli/tests/query_sql.rs`: `query sql` through `run()` against a mock
  server, covering each exit code, partial causes in JSON, table and CSV, credentials
  sent but never printed, conflicts sending nothing, failing writers (exit `8`), and
  the fallback history on a failure after fallback.
- `crates/packdb-cli/tests/query_languages.rs`: `query promql`, `query logql`,
  `query traceql` and `trace` through `run()` against a mock server, covering the
  request each sends (times, windows, step, limit, direction, credentials and
  tenant), table and CSV columns, completeness for each command, the found and
  missing trace, server warnings and infos, malformed answers in every format, each
  exit code, and usage errors that send nothing.
- `crates/packdb-cli/tests/doctor.rs` and the `probe` and `report` unit tests:
  `doctor` through `run()` against a mock server, sending only `GET /info` and
  `GET /health/diagnostics` with the credential and tenant, health and readiness from
  the diagnostics route (healthy, degraded, unhealthy, not ready, absent, unusable), withheld and not-exposed checks keeping the report
  incomplete, unusable `/info` answers (wrong shape, partial, redirect, throttle),
  `401` and `403`, an unreachable or silent server, one deadline across retries,
  and a long `Retry-After` reported without retrying.
- `crates/packdb-cli/tests/smoke.rs` and the `smoke` unit tests: nothing sent without
  `--allow-write`; each signal written once with every read scoped to the run's marker
  or trace ID; rejected, malformed, unrecognized and mislabelled acknowledgements; a
  `429` write not retried; a refused credential stopping the writes; an unreachable
  server; data that never appears failing at the timeout.
- `crates/packdb-cli/tests/bundle.rs` and the `bundle` and `secrets` unit tests: a
  bundle holding the server's answers but never the credential; a server answer
  carrying the credential, a short or JSON-escaped credential, or a private key left
  out of the archive; an existing output refused before anything is sent; an
  unreachable server still bundled (exit `6`); an unwritable destination (exit `8`);
  every secret class; a manifest that would carry the credential writing nothing; a
  failed write leaving other files, including look-alike temporary names, untouched;
  a deterministic tar read back by the system `tar`.
- `crates/packdb-cli/tests/usage.rs` and the `usage` unit tests: one `GET
  /health/usage` with the credential, each measurement's scope and period kept, an
  unavailable value reported as missing evidence and never as zero, a server without
  the route, answers with a wrong schema, scope, period, omitted or negative value,
  `401`, `403` and an unreachable server.
- `crates/packdb-cli-test-harness/tests/process.rs`: the same contract as a process
  sees it, covering help, the refused secret argument, stdout/stderr separation,
  exit codes, credential files, conflicts, a closed stdout (exit `8`), the PromQL,
  trace and time-argument cases, `doctor` and `usage-report`.
- `packdb-client`'s own tests cover the transport fallback rules, the query API
  requests and error classification.

All run on Rust 1.94.1 with default features and with `flight-sql`, against the
pinned sibling SDK revision, without a PackDB server.
