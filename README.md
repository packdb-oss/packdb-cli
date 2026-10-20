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

Source and binary releases are available from [GitHub Releases](https://github.com/packdb-oss/packdb-cli/releases).
The package is not published to crates.io. Its manifest keeps `publish = false`;
registry publication is a separate reviewed release action and does not follow
automatically from repository visibility.

[`crates/packdb-query-cli/COMPATIBILITY.md`](crates/packdb-query-cli/COMPATIBILITY.md)
is the binding contract: flags, output formats, transport selection, exit codes and
the release matrix. It ships inside the crate rather than at the repository root, so
the copy a consumer receives is the copy this repository tests.

Released binaries build two native targets — `x86_64-unknown-linux-gnu` and
`aarch64-apple-darwin` — each on a runner of its own architecture, with `flight-sql`
enabled. Nothing is cross-compiled: a cross-built binary's runtime floor is a
property of the sysroot it linked against, which is precisely what the declared
glibc 2.35 floor exists to establish independently.

## Building from source

The CLI depends on `packdb-client` **by version**. While that crate is unavailable
on crates.io, the workspace resolves it through a patch to a sibling SDK checkout:

```
<parent>/packdb-sdk/     # the SDK export
<parent>/packdb-cli/     # this repository
```

Without that sibling, every cargo command that resolves dependencies fails with
`failed to load source for dependency 'packdb-client'`. This is expected, not a
defect. `scripts/oss-ci.sh` states it plainly rather than letting cargo's message
stand alone. Use the SDK revision pinned by `SDK_COMMIT` in
[the CI workflow](.github/workflows/oss-ci.yml). Remove the `[patch.crates-io]`
section only when the required SDK version is available on crates.io and a clean
registry build passes; making the SDK repository public is not sufficient.

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
