# `packdb-query-cli`

Command-line SQL client for PackDB, installed as the `packdb-query`
binary.

Note the package and binary names differ deliberately: the package is
`packdb-query-cli`, the executable it installs is `packdb-query`.

Its supported surface — flags, environment variables, exit codes and
stderr prefixes — is written down in
[`COMPATIBILITY.md`](./COMPATIBILITY.md), including what is explicitly
*not* promised.

## Status

The CLI is maintained in [packdb-cli](https://github.com/packdb-oss/packdb-cli).
Source and binary archives are available from that repository's GitHub Releases.
The crate is not published to crates.io and keeps `publish = false` until registry
publication is approved. Source builds need the sibling SDK checkout described
in the [repository README](../../README.md).

## Licence

Apache-2.0. See [`LICENSE`](../../LICENSE) at the workspace root.
