# PackDB command library

`packdb-cli` is the command library behind `packdb`, PackDB's command-line
client. It implements `packdb query sql|promql|logql|traceql`, `packdb trace` and
`packdb doctor`; the other operator commands follow.

| Package | Scope |
| --- | --- |
| `packdb-cli` | The command library: arguments, credentials, output, completeness and exit codes |
| `packdb-cli-test-harness` | Test-only executable that runs the library as a process; never released |

This repository ships no executable. The `packdb` binary that embeds the library
is assembled with the server. `packdb-query`, the binary of releases 0.2.0 to
0.2.4, is retired; its archives remain on
[GitHub Releases](https://github.com/packdb-oss/packdb-cli/releases) unchanged.

Server implementations, control-plane access, credential issuance, tenant
resolution, admission, placement, routing and other commercial service contracts
are outside this repository.

## Status and use

The package is not published to crates.io. Its manifest keeps `publish = false`;
registry publication is a separate reviewed release action and does not follow
automatically from repository visibility.

[`crates/packdb-cli/COMPATIBILITY.md`](crates/packdb-cli/COMPATIBILITY.md) is the
binding contract: commands, connection and credential rules, transport, output,
completeness, exit codes, and how to migrate from `packdb-query`. It ships inside
the crate rather than at the repository root, so the copy a consumer receives is
the copy this repository tests.

## Building from source

The CLI depends on `packdb-client` **by version**. While that crate is unavailable
on crates.io, the workspace resolves it through a patch to a sibling SDK checkout:

```
<parent>/packdb-sdk/     # the SDK export
<parent>/packdb-cli/     # this repository
```

Without that sibling, Cargo cannot resolve `packdb-client`. Use the SDK revision
pinned by `SDK_COMMIT` in
[the CI workflow](.github/workflows/oss-ci.yml). Remove the `[patch.crates-io]`
section only when the required SDK version is available on crates.io and a clean
registry build passes; making the SDK repository public is not sufficient.

Use Rust 1.94.1 and test from the CLI checkout:

```sh
cargo test --locked --workspace --all-targets --features flight-sql
```

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
