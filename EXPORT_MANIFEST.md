# Export manifest

This repository is **generated**, not authored. Its sources are assembled
from one commit of the private PackDB monorepo, so this export can be
regenerated and diffed against what was published rather than trusted from
prose.

`Cargo.lock` is a reviewed file carried by the export kit, copied in
unchanged rather than resolved against crates.io at export time. Registry
state is therefore not an input: the same commit and the same kit produce
the same tree whenever and wherever the export is run.

Do not edit these sources here. A change belongs upstream and reaches this
repository by regenerating the export; an edit made here is silently
reverted by the next regeneration.

## Provenance

| | |
| --- | --- |
| source commit | `1d26a87713eeef94c6044003f6b8c14a8812f224` |
| export kit | `1d26a87713eeef94c6044003f6b8c14a8812f224` |
| repository | `packdb-cli` |
| members | 1 |
| files | see `FILE_INVENTORY.txt` |
| checksums | see `SHA256SUMS` |

## Members

- `packdb-query-cli` (`crates/packdb-query-cli`), from `apps/packdb-query`

## What the export does to the sources

1. **Extracted** from the commit with `git archive`, never from a working
   tree, so uncommitted local state cannot leak in.
2. **Inspected by the sanitiser.** When a source needs an internal citation
   removed, the edit is anchored and must match exactly once. This target
   currently expects `0` source edit(s); the denylist
   still scans the complete result when that count is zero.
3. **Manifests rewritten** so nothing inherits from the monorepo workspace.
   Versions come from that workspace except for the reviewed SDK-only
   dependency map below. `Cargo.lock` is then copied from the kit's reviewed
   copy, after checking it against a fingerprint of these manifests.
4. **Overlaid** with files that exist only in the export. For this SDK, that
   includes the licence, standalone workspace, toolchain pin, community policy,
   repository CI adapter and fail-closed workflows. Keeping them in the overlay
   makes a regenerated repository reproduce its governance and checks too.
5. **Refused** if any denylisted pattern survives -- internal citations,
   internal crate names, internal hosts, or leftover workspace inheritance.

## Deliberate decisions a reader should not mistake for oversights

- **`publish = false` is retained.** This tree is a candidate. Clearing that
  flag is part of an authorised publication, not part of building one.
- **Repository metadata names `packdb-oss/packdb-sdk`.** Repository creation is
  still a separate owner action; recording the decided destination here keeps
  eventual package metadata reproducible.
- **`packdb-client` documents what it excludes** -- credential issuance,
  tenant resolution, control-plane access. That names internal concepts
  because it is a boundary statement, and removing it would make the public
  scope less legible rather than more.
- **Each member carries its own `LICENSE` copy.** `cargo package` includes
  only files inside a crate directory, so a root-only licence would ship
  with the repository but not with the published crate.
