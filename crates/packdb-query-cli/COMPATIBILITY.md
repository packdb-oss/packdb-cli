# `packdb-query` compatibility contract

What a released `packdb-query` binary promises its callers. Written before the
crate is extracted, so the promise is decided here rather than inferred later
from whatever a build happened to produce.

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

`integration-tests/tests/cli/packdb_query_cli_test.rs` already splits along the
line extraction needs:

1. **Argument parsing and validation — no server.** `--help`, `--version`, the
   missing-SQL case, invalid `--format`, every valid `--format`, conflicting
   auth options, and both environment variables. This half is portable and can
   run in an extracted repository exactly as the parser corpora do.
2. **Query execution — `#[ignore]`, requires a running PackDB server.** This
   half cannot move, for the same reason parser testing must never wait on a
   deployed server.

Half 1 is where this contract is checked. A change to a flag name, a default,
the precedence order, the format list or an exit code fails there — but *when*
it fails is not what a reader assumes, so be precise about it.

These live in `packdb-integration-tests`, and `make ci-core` builds only
`--lib --bins`. On a pull request they are therefore *compiled but not run*:
CI Core's "Check integration-test targets compile (warning-clean)" step covers
them, which catches a rename or a signature change but not a behaviour change.
They are *executed* on the nightly default-branch schedule, the weekly full
sweep, and on demand via `/run-integration-tests`.

So a PR that silently changed an exit code would compile green and be caught
that night, not at review time. That is the repository's deliberate policy for
integration suites rather than a gap in this file — but a reader deciding how
much weight to put on "the contract is tested" should know which of the two it
is.

### What half 1 now asserts, and what it still cannot

Half 1 used only to assert that valid invocations *parse* — several tests ended
in `.failure()` because no server was listening and treated that as success,
without checking **which** failure. The exit code is the part scripts actually
depend on, so this change added server-free assertions for each documented
class: connection refused exits `2` with `ERROR:`, `--format bogus` and
`--format JSON` exit `2` with `error:`, an unknown flag exits `2`, `--help` and
`--version` exit `0`, the documented auth flag names are accepted, and the
advertised transport flags match what the parser accepts.

That last one is an invariant rather than a fixed expectation: it reads
`--help`, and for each transport flag asserts the parser agrees with what the
help text advertises. Written as a `#[cfg(feature = "flight-sql")]` test it
would have gated on the *test crate's* feature rather than how the binary was
built, and compiled out silently in CI — which is how it was written first.

Still not assertable without a server, and so deliberately left out:

- **Auth precedence.** The tests confirm each flag is accepted; proving
  api-key beats token beats basic-auth means observing the outgoing header.
- **`--timeout` behaviour**, which needs a server that is slow on purpose.
- **stdout byte-exactness**, which needs a result to print.

These belong to half 2, and stay internal for the same reason it does.
