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

## Quickstart

You need an existing PackDB endpoint and credentials issued for that endpoint.
Installing this client does not start a PackDB server, create an account, or issue
credentials.

### Download and verify

The commands below use release **0.2.4**. Choose the target for your machine:

| Platform | `target` value | Requirement |
| --- | --- | --- |
| macOS on Apple silicon | `aarch64-apple-darwin` | The archive is not notarized |
| Linux on x86_64 | `x86_64-unknown-linux-gnu` | glibc 2.35 or newer |

Both released binaries include Flight SQL. Windows, Intel macOS, Linux ARM64,
and musl binaries are not provided. On macOS, follow your organization's software
approval policy; a source build is an alternative to the unnotarized archive.

In a fresh download directory, set `target` to the appropriate value, then run:

```sh
version=0.2.4
target=aarch64-apple-darwin
archive="packdb-query-${version}-${target}.tar.gz"
release="https://github.com/packdb-oss/packdb-cli/releases/download/v${version}"

curl --fail --location --output "$archive" "$release/$archive" &&
curl --fail --location --output "$archive.sha256" "$release/$archive.sha256" &&
shasum -a 256 -c "$archive.sha256" &&
tar -xzf "$archive" &&
"./packdb-query-${version}-${target}/packdb-query" --version
```

On Linux, `sha256sum -c "$archive.sha256"` can replace the `shasum` command.
Stop on a checksum mismatch. The checksum verifies the download against the
published checksum, not an independent signature. The extracted directory also
contains the license, third-party notices, compatibility contract, and provenance.
Keep those files with the binary when redistributing it.

### Run a first query

Load `PACKDB_API_KEY` into the process environment through your secret manager;
do not paste a key into a command or commit it to a file. The following uses an
operator-provided local endpoint. Replace it with your assigned HTTPS URL for a
remote service. Run from the same download directory with `version` and `target`
still set:

```sh
export PACKDB_URL='http://127.0.0.1:4000'
: "${PACKDB_API_KEY:?Load your issued API key into the environment first}"
"./packdb-query-${version}-${target}/packdb-query" \
  --http-only --format json 'SELECT 1 AS connected'
```

The result should contain `connected: 1`. Use `--format table` for a readable
table or `--format csv` for CSV. Query output goes to stdout; errors go to stderr.
An exit status of `0` reports a successful query, not a telemetry-completeness
guarantee: the CLI's formatted output does not expose upstream partial-result
causes. Use the [SDK's completeness-preserving API](https://github.com/packdb-oss/packdb-sdk#quickstart-query-over-http)
when your application needs to reject partial data.

For Flight SQL, have your operator supply `PACKDB_FLIGHT_URL`, remove `--http-only`,
and pass `--flight-endpoint "$PACKDB_FLIGHT_URL"`. Flight is preferred and HTTP is
the fallback; the client does not create or discover a Flight endpoint.

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

Use Rust 1.94.1 and build from the CLI checkout:

```sh
cargo build --locked --release --features flight-sql
```

Release binaries are built natively on each supported platform. The release
workflow checks the Linux binary's required glibc symbols against the declared
2.35 floor; a local source build does not independently establish that floor.

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
