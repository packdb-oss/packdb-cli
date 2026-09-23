# Releasing

Only named release maintainers may publish. The package remains `publish = false`
during private staging. Repository visibility does not make crates.io private.

## Order of operations

`packdb-query-cli` depends on `packdb-client` by version. It cannot be published,
and cannot resolve from the registry, until `packdb-otel` and then `packdb-client`
are on crates.io. Binary releases can follow either way, because a binary archive
is built from source and does not need the registry — but a crates.io release of
this package does. Do not attempt them in the other order.

## Private candidates

1. Select a version and update this changelog and the compatibility contract.
2. Run full checks, coverage, compatibility, and security gates on the exact commit.
3. Inspect `cargo package --list`. The package must include its README, LICENSE and
   COMPATIBILITY.md, and exclude private data and internal-only source.
4. Dispatch `oss-candidate`. It builds both native targets from one shared source
   state, extracts every archive and exercises the shipped binary — help, version,
   both transport flags and every documented exit code — and holds the Linux binary
   to the declared glibc floor by reading its required `GLIBC_*` symbols.
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

Binary archives attach to a GitHub Release. Publish only archives a candidate run
produced and verified; never a locally built one. Each archive carries its
provenance, dependency inventory, licence and the compatibility contract, and its
checksum is published alongside it.

Before public visibility, review reachable history, Actions artifacts and private
release attachments; verify security and conduct contacts; name a second maintainer;
and document registry recovery. Describe capabilities and observed limitations
without maturity or production-quality claims. “Used in production” describes use
only and provides no quality, HA, fitness, support, or commercial guarantee.
