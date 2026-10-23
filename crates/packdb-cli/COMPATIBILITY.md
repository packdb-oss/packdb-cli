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
```

`packdb` with no subcommand prints help to stderr and exits `2`; it never starts a
server. `--help` and `--version` exit `0`.

### Connection and credentials

| flag | environment | default |
| --- | --- | --- |
| `--endpoint URL` | `PACKDB_ENDPOINT` | `http://127.0.0.1:4000` |
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

### Output

- **stdout carries the result and nothing else.** Warnings, notes and errors go to
  stderr, prefixed `warning: `, `note: ` or `error: `.
- `--format table` (default) and `--format csv` print the rows. `--format json`
  prints one JSON object:

  ```json
  {
    "schema_version": "packdb.cli.v1",
    "command": "query sql",
    "endpoint": "http://127.0.0.1:4000",
    "transport": {"requested": "auto", "used": "http", "fallback_reason": null},
    "completeness": "unknown",
    "partial_causes": [],
    "warnings": ["completeness unknown (...)"],
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

### Completeness

`completeness` is one of:

- `partial`: the server reported causes (`x-packdb-partial-causes`, or Flight SQL
  initial metadata). They are listed in `partial_causes`.
- `complete`: only where the protocol's documented and verified contract says an
  absent cause list means complete, and none was reported.
- `unknown`: everything else. Unknown is never reported as complete.

For `query sql`, an absent cause list is **`unknown`**: the server's SQL path has not
yet been verified to mark every incomplete result. In table and CSV output, a
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
| `7` | server or protocol failure (any other non-2xx, including every `5xx`) |
| `8` | output could not be delivered: writing or flushing stdout or stderr failed (a full disk, a closed pipe), including warnings and error messages |

When several apply, the first match in the order `8, 2, 4, 5, 6, 7, 1, 3` wins. A
result that was not fully written never exits `0` or `3`. Over
HTTP a `400` whose body is the server's query or parse rejection (`Query error: ...`
or `Parse error: ...`) is `1`; over Flight SQL, `InvalidArgument` is `1`.

## Not promised

- **Exact table layout.** Parse `json` or `csv` instead.
- **Error and warning text** after the `error: ` / `warning: ` prefix.
- **`--help` and `--version` formatting.**
- **Ordering of result rows**, unless the SQL orders them.
- **JSON key order and whitespace.**

## Migrating from `packdb-query`

| `packdb-query` (0.2.x) | `packdb` |
| --- | --- |
| `packdb-query '<SQL>'` | `packdb query sql '<SQL>'` |
| `--url` / `PACKDB_URL` | `--endpoint` / `PACKDB_ENDPOINT` |
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
- `crates/packdb-cli-test-harness/tests/process.rs`: the same contract as a process
  sees it, covering help, the refused secret argument, stdout/stderr separation,
  exit codes, credential files, conflicts and a closed stdout (exit `8`).
- `packdb-client`'s own tests cover the transport fallback rules and error
  classification.

All run on Rust 1.94.1 with default features and with `flight-sql`, against the
pinned sibling SDK revision, without a PackDB server.
