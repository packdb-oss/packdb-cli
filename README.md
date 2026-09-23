# PackDB query CLI

`packdb-query` is a single-shot SQL client for PackDB. It sends one statement over
HTTP or Flight SQL, prints the result, and exits.

| Package | Scope |
| --- | --- |
| `packdb-query-cli` | The `packdb-query` binary: query command, flags, output formats and exit-code contract |

Server implementations, control-plane access, credential issuance, tenant
resolution, admission, placement, routing and other commercial service contracts
are outside this repository.

## Status and use

The package is staged privately and is not published to crates.io. Its manifest keeps
`publish = false`; changing that is a separate reviewed release action. Until a public
release exists, this repository is source and candidate evidence rather than a
supported installation channel.

[`crates/packdb-query-cli/COMPATIBILITY.md`](crates/packdb-query-cli/COMPATIBILITY.md)
is the binding contract: flags, output formats, transport selection, exit codes and
the release matrix. It ships inside the crate rather than at the repository root, so
the copy a consumer receives is the copy this repository tests.

Released binaries build two native targets — `x86_64-unknown-linux-gnu` and
`aarch64-apple-darwin` — each on a runner of its own architecture, with `flight-sql`
enabled. Nothing is cross-compiled: a cross-built binary's runtime floor is a
property of the sysroot it linked against, which is precisely what the declared
glibc 2.35 floor exists to establish independently.

## Building before the SDK is published

The CLI depends on `packdb-client` **by version**. Until that crate reaches
crates.io there is no registry to resolve it from, so the workspace patches it to a
sibling checkout:

```
<parent>/packdb-sdk/     # the SDK export
<parent>/packdb-cli/     # this repository
```

Without that sibling, every cargo command that resolves dependencies fails with
`failed to load source for dependency 'packdb-client'`. This is expected, not a
defect. `scripts/oss-ci.sh` states it plainly rather than letting cargo's message
stand alone. Delete the `[patch.crates-io]` section once the SDK is published.

## Development

```sh
OSS_CHECK_SCOPE=full scripts/oss-ci.sh checks
OSS_CHECK_SCOPE=full scripts/oss-ci.sh compatibility
OSS_CHECK_SCOPE=full scripts/oss-ci.sh coverage
OSS_CHECK_SCOPE=full scripts/oss-ci.sh security
```

See [CONTRIBUTING.md](CONTRIBUTING.md), [CODING_STANDARDS.md](CODING_STANDARDS.md)
and [AI_POLICY.md](AI_POLICY.md) before contributing.

## License

Apache License 2.0; see [LICENSE](LICENSE). The package carries its own copy of the
license so its eventual crate archive is self-contained.
