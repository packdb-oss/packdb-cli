# Releasing

Only named release maintainers may publish. The package remains `publish = false`
until registry publication is explicitly enabled and qualified. Repository visibility,
GitHub releases and crates.io publication are separate decisions; crates.io is public.

## Order of operations

`packdb-cli` depends on `packdb-client` by version. It cannot be published, and
cannot resolve from the registry, until `packdb-otel` and then `packdb-client` are
on crates.io. Do not attempt them in the other order.

This repository releases source only. The `packdb` binary that embeds the library
is built and released with the server; `packdb-query` binary archives of 0.2.0 to
0.2.4 stay on their GitHub Releases unchanged.

## Private candidates

1. Select a version and update this changelog and the compatibility contract.
2. Run full checks, coverage, compatibility, and security gates on the exact commit.
3. Inspect `cargo package --list`. The package must include its README, LICENSE and
   COMPATIBILITY.md, and exclude private data and internal-only source.
4. Dispatch `oss-candidate`. It reruns every gate on the exact commit, including
   the test harness's process-level contract tests, and records the source archive,
   package and source inventories, dependency inventory, checksums and the SDK
   revision it was built against.
5. Review the source archive, dependency inventory, checksums and release record. A
   candidate does not authorize a tag, GitHub Release, visibility change, or
   registry upload.

## Publication

Publication requires a separate reviewed change that enables only an explicit
package allowlist and configures registry ownership. Verify the registry checksum
and perform a clean consumer build. Never overwrite or blindly retry a version.

Use crates.io trusted publishing scoped to this repository, workflow, and protected
`public-release` environment when available. The publish workflow stays disabled
while `OSS_PUBLIC_RELEASE_ENABLED` is not `true`, and its adapter refuses publication
until the launch implementation is reviewed. Do not use `pull_request_target` or
`workflow_run` for publication.

Attach to a GitHub Release only the source archive and records a candidate run
produced and verified; never a locally built archive.

Before public visibility, review reachable history, Actions artifacts and private
release attachments; verify security and conduct contacts; record the release-owner review;
and document registry recovery. Describe capabilities and observed limitations
without maturity or production-quality claims. “Used in production” describes use
only and provides no quality, HA, fitness, support, or commercial guarantee.
