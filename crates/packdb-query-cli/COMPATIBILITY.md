# `packdb-query` compatibility contract

What a released `packdb-query` binary promises its callers. This contract is
maintained with the public CLI and included in its released archives.

Read as: anything in **The contract** changes only with a deliberate,
documented decision. Anything under **Not promised** may change without one.

## The contract

### Invocation

```
packdb-query [OPTIONS] <SQL>
```

`<SQL>` is a single positional argument and is required.

### Flags and environment

| flag | environment | default |
| --- | --- | --- |
| `--url` | `PACKDB_URL` | `http://localhost:4000` |
| `--api-key` | `PACKDB_API_KEY` | — |
| `--token` | `PACKDB_TOKEN` | — |
| `--basic-auth` | — | — |
| `--timeout` | — | `30` (seconds) |
| `--format` | — | `table` |
| `--flight-endpoint` | `PACKDB_FLIGHT_URL` | — |
| `--http-only` | — | off |

A flag always wins over its environment variable.

### Authentication precedence

When more than one credential is supplied, the **first** of these wins and the
rest are ignored silently:

1. `--api-key` / `PACKDB_API_KEY`
2. `--token` / `PACKDB_TOKEN`
3. `--basic-auth`
4. none

`--basic-auth` takes `user:password`.

This is first-match, not an error. A caller passing both a token and an API key
gets the API key with no warning. That behaviour is part of the contract because
scripts depend on it; it is not an invitation to pass two.

### Output format

`--format` accepts exactly `table`, `json` or `csv`, **case-sensitively**.
`JSON` and `Json` are rejected.

The argument parser validates the value against that list before the client's
own parser sees it, and the client's parser lowercases — so the
case-insensitivity in `OutputFormat::parse` is unreachable from the CLI. Callers
must not rely on it. Verified against a built binary: `--format JSON` exits `2`
with `error: invalid value 'JSON' for '--format <FORMAT>'`.

- `table` — ASCII table, human-readable
- `json` — machine-readable
- `csv` — spreadsheet-compatible

### Streams

- **stdout** carries the query result and nothing else. It is written
  byte-for-byte: no trailing newline is added.
- **stderr** carries errors, in the form `ERROR: <message>\n`.

A caller may pipe stdout without filtering.

### Exit codes

PostgreSQL-style. Defined in `packdb-client`'s `ClientError::exit_code`, which
means **the exit-code contract spans two crates** — a change there changes this
contract even though the CLI's own source is untouched.

| code | meaning |
| --- | --- |
| `0` | success |
| `1` | SQL error — the server rejected or failed the query |
| `2` | connection, configuration or I/O error |
| `3` | protocol error |

The split that matters to a script: `1` means the query was wrong, `2` means the
invocation or the environment was wrong, `3` means the transport misbehaved.

Over HTTP the class comes from the server's response. A `400` whose body is the
server's query or parse rejection (`Query error: ...` or `Parse error: ...`) is
`1`; `401` and `403` are `2`; every other non-2xx response, including every `5xx`,
is `3` until its meaning is established. A Flight SQL failure falls back to
HTTP, so a query rejected after that fallback is still `1`. Version 0.2.0
reported every non-2xx response as `3`, including invalid SQL.

`--help` and `--version` exit `0`. Argument-parsing failures — an unknown flag,
a missing `<SQL>`, an invalid `--format` — exit `2`, the same class as a
connection failure. A script cannot tell "I typed the flag wrong" from "the
server is down" by exit code alone; it must read stderr, where the parser's
message begins `error:` and the client's begins `ERROR:`.

Measured against a built binary rather than read off the source: connection
refused `2`, `--format JSON` `2`, `--format bogus` `2`, missing `<SQL>` `2`,
`--help` `0`, `--version` `0`.

The binary's own `--help` epilogue used to describe code `2` as "Connection
error (cannot reach server or authentication failed)", which is narrower than
what it actually covers — it says nothing about a mistyped flag. It now reads
"Invalid arguments, configuration, connection or I/O failure" and names the two
stderr prefixes, so the binary and this table agree.

### Transport selection

Released binaries are built **with the `flight-sql` feature on every target**
(see *Release matrix*), so `--flight-endpoint` and `--http-only` are always present.

- With `--flight-endpoint` set and `--http-only` absent, Flight SQL is preferred.
- `--http-only` forces HTTP and skips Flight SQL.

A build without the feature pins the client to HTTP and removes both flags.
Such a build is **not a released artifact** and does not satisfy this contract —
it presents a different flag surface, which is exactly the divergence this
section exists to prevent.

### Release matrix

| target | archive |
| --- | --- |
| `x86_64-unknown-linux-gnu` | `.tar.gz` |
| `aarch64-apple-darwin` | `.tar.gz` |

Two targets, each built on a runner of its own architecture. **Nothing here is
cross-compiled**, deliberately: a cross-built binary's real runtime floor is a
property of the sysroot it was linked against, and that is precisely what the
glibc assertion below is meant to establish independently rather than assume.

Each archive ships with SHA-256 checksums and source provenance.

**The Linux binary targets a declared glibc 2.35 baseline.** Declared, not
inherited: the floor is a decision recorded here, never whatever glibc the
build runner happened to have. A runner image upgrade must not move it silently.
The released binary's required `GLIBC_*` symbols are read back out of the
extracted archive and checked against that floor, so the claim is asserted
rather than stated. State the floor in the installation notes.

Not in scope for the first release: `aarch64-unknown-linux-gnu`,
`x86_64-apple-darwin`, musl, containers, installers, package-manager channels,
Windows. Those two targets are named explicitly because an earlier revision of
this document promised them; a reader who saw that revision should not assume
they merely went unlisted.

## Not promised

- **Exact table layout.** Column widths, padding and separators in `table`
  output are presentation, not contract. Parse `json` or `csv` instead.
- **Error message text.** The `ERROR: ` prefix, the stream and the exit code are
  contract; the wording after the prefix is not.
- **`--help` and `--version` formatting.** Both exist and exit `0`; their layout
  comes from the argument parser.
- **Ordering of result rows**, unless the SQL itself orders them.
- **Behaviour with more than one credential**, beyond the precedence above.

## How this is tested

The public repository's [CI workflow](../../.github/workflows/oss-ci.yml) runs
`checks`, `coverage`, and `compatibility` on pull requests, pushes to `main`, and
merge-group revisions, as well as scheduled and manual runs. These are executed
checks, not a compile-only promise deferred to a nightly server suite.

The [compatibility adapter](../../scripts/oss-ci.sh) runs the CLI's tests on Rust
1.94.1 with default features and with `flight-sql`, builds a release binary with
Flight SQL, and runs the
[shipped-contract script](../../scripts/assert-shipped-contract.sh) against it.
The candidate workflow runs the same script against the binary extracted from
each platform archive. The checks use the reviewed sibling SDK revision pinned
by the workflow; they do not require a deployed PackDB server.

### What public checks cover

- **Configuration and argument handling:** unit tests in `src/main.rs` cover
  default URL/timeout/output, supported formats, malformed basic authentication,
  API-key precedence over malformed basic authentication, Flight endpoint wiring,
  `--http-only`, and the HTTP-only configuration without the Flight feature.
- **Built-binary checks:** the shipped-contract script checks `--help` and
  `--version` exit `0`; missing SQL, invalid format values, and a refused
  connection exit `2`; and both released transport flags appear in help.
- **Release matrix:** the script checks that this contract includes the two
  released targets and does not reintroduce the withdrawn Linux ARM64 or Intel
  macOS targets. Native candidate jobs and the Linux glibc check provide the
  corresponding artifact evidence.

### Limits of those checks

The shipped-contract script does **not** prove every documented exit-code class:
it exercises `0` and `2`, not server-query rejection (`1`) or protocol failure
(`3`). It also does not assert stderr prefixes or stdout byte-exactness. Its
summary must not be read as broader evidence than its individual assertions.

The configuration tests pin selected precedence cases, not a complete test of
outgoing authentication headers. End-to-end evidence for credential handling,
server rejection/protocol responses, timeout behavior, formatted results, and
Flight-to-HTTP fallback requires an appropriate test server or controlled stub.
Server integration evidence is separate from this public CLI's server-free gate;
passing that gate alone makes no claim about a deployed PackDB service.
