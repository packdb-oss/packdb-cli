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

Part of the [PackDB](https://packdb.io) workspace. **Not published to
crates.io**, and `publish = false` in its manifest: it is on an extraction
path whose repository boundary has not been decided yet. Depend on it by
path within this workspace.

## Licence

Apache-2.0. See [`LICENSE`](../../LICENSE) at the workspace root.
