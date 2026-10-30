# `packdb-cli`

`packdb`, PackDB's command-line client, and the command library behind it:
queries, trace lookup, limited `doctor` diagnostics, `smoke-test`,
`support-bundle` and `usage-report` against a running PackDB server.

The crate builds the `packdb` binary and exports the library it runs (`Command`,
`run`, `Io`). `packdb` never starts a server; the single-node server is
`packdb-server`, built from the server repository. `packdb-query`, the binary of
releases 0.2.0 to 0.2.4, is retired; its released archives are unchanged.

The supported surface — arguments, environment variables, output, completeness
and exit codes — is written down in [`COMPATIBILITY.md`](./COMPATIBILITY.md),
including what is explicitly *not* promised and how to migrate from
`packdb-query`.

## Status

Maintained in [packdb-cli](https://github.com/packdb-oss/packdb-cli). The crate is
not published to crates.io and keeps `publish = false` until registry
publication is approved. Builds need the sibling SDK checkout described in the
[repository README](../../README.md).

## Licence

Apache-2.0. See [`LICENSE`](./LICENSE).
