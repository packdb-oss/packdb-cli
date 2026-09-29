# Provenance

This repository is the **canonical source** of `packdb-query-cli` (the `packdb-query` binary). Changes are authored,
reviewed and released here.

It began as an export from the private PackDB monorepo. The last export was
generated from monorepo commit `b497879865b45b60998a453cd4bd77e3eb81b271` and released as
`v0.2.1` (`1449d9bb7a0ccc84d51d8e0e5b73e19bfcde9491`). The monorepo's export kit was then removed
(packdb/packdb#1731), so nothing regenerates this tree and nothing reverts an
edit made here.

`Cargo.lock` is a reviewed file: dependency versions change only through a
reviewed pull request -- Dependabot's or a maintainer's -- never at build time.

Release provenance is recorded per release. The `oss-candidate` workflow builds
from the exact commit to be tagged and publishes a `release-record.json` and
`SHA256SUMS` with every candidate; those files, not a repository-root
inventory, identify what a release contains. The export-time
`FILE_INVENTORY.txt` and `SHA256SUMS` were removed with the kit because they
described the last export, not the current tree.

This file keeps its name because the repository's policy check requires it.

## Deliberate decisions a reader should not mistake for oversights

- **`publish = false` is retained.** Clearing that flag is part of an
  authorised publication, not part of building a release.
- **`packdb-client` and `packdb-otel` come from a pinned `packdb-sdk` commit**
  (`SDK_COMMIT`, see `docs/WORKFLOWS.md`) until the SDK is published; they
  move only when that pin is bumped in a reviewed pull request.
- **Each member carries its own `LICENSE` copy.** `cargo package` includes
  only files inside a crate directory, so a root-only licence would ship
  with the repository but not with the published crate.
