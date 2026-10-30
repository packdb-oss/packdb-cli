#!/usr/bin/env bash
# Copyright 2025 PackDB Contributors
# Licensed under the Apache License, Version 2.0
# Repository adapter for docs/WORKFLOWS.md.

set -euo pipefail

readonly MSRV="1.94.1"
readonly REPOSITORY_URL="https://github.com/packdb-oss/packdb-cli"
readonly CARGO_LLVM_COV_VERSION="0.9.1"
readonly CARGO_AUDIT_VERSION="0.22.2"
readonly CARGO_DENY_VERSION="0.20.2"
readonly GITLEAKS_VERSION="v8.30.1"
readonly PACKAGES=(packdb-cli)
# The contract ships INSIDE the crate, not at the repository root: the copy a
# consumer receives is then the copy this repository tests, and there is no
# second one free to drift.
readonly CONTRACT="crates/packdb-cli/COMPATIBILITY.md"

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

die() { echo "oss-ci: $*" >&2; exit 1; }
log() { echo "==> $*"; }
is_sha() { [[ "$1" =~ ^[0-9a-f]{40}$ ]] && [[ "$1" != 0000000000000000000000000000000000000000 ]]; }

scope() {
    case "${OSS_CHECK_SCOPE:-}" in
        affected | full) echo "$OSS_CHECK_SCOPE" ;;
        *) die "OSS_CHECK_SCOPE must be 'affected' or 'full', got '${OSS_CHECK_SCOPE:-}'" ;;
    esac
}

resolve_commit() {
    local name="$1" sha="$2"
    is_sha "$sha" || die "$name is not a usable commit SHA: '${sha}'"
    if ! git cat-file -e "${sha}^{commit}" 2>/dev/null; then
        git fetch --no-tags --depth=1 origin "$sha" >/dev/null 2>&1 ||
            die "$name $sha is not available and could not be fetched"
    fi
    git cat-file -e "${sha}^{commit}" 2>/dev/null || die "$name $sha is not a commit"
}

changed_files() {
    local out="$1"
    if [[ "$(scope)" == full ]]; then
        git ls-files >"$out"
        return
    fi
    local base="${OSS_BASE_SHA:-}" head="${OSS_HEAD_SHA:-}" from
    resolve_commit OSS_BASE_SHA "$base"
    resolve_commit OSS_HEAD_SHA "$head"
    from="$base"
    if [[ "${GITHUB_EVENT_NAME:-}" != push ]]; then
        from="$(git merge-base "$base" "$head")" || die "no merge base for $base..$head"
    fi
    git diff --name-only --no-renames "$from" "$head" >"$out"
    echo "tested checkout: $(git rev-parse HEAD); range: ${from}..${head}"
}

check_gate_input() {
    local expected="$1"
    [[ -z "${OSS_GATE:-}" || "$OSS_GATE" == "$expected" ]] ||
        die "OSS_GATE='${OSS_GATE}' disagrees with subcommand '${expected}'"
}

require_tool() { command -v "$1" >/dev/null 2>&1 || die "required tool '$1' is not installed"; }

install_cargo_tool() {
    local crate="$1" version="$2" binary="$3"
    if command -v "$binary" >/dev/null 2>&1 && cargo "${binary#cargo-}" --version 2>/dev/null | grep -qF " $version"; then
        return
    fi
    [[ -n "${CI:-}" ]] || die "$crate $version is required; install it with: cargo install --locked $crate@$version"
    cargo install --locked "$crate@$version"
}

# The one structural difference from the SDK adapter.
#
# `packdb-cli` depends on `packdb-client` BY VERSION. Until that crate is
# published there is no registry to resolve it from, so the workspace patches it
# to a sibling checkout. EVERY gate here resolves dependencies -- clippy, the
# test matrix, llvm-cov, cargo-deny's metadata read -- so every gate needs the
# sibling. Cargo's own message for its absence is `failed to load source for
# dependency 'packdb-client'`, which does not tell a reader what to do about it.
require_sdk_sibling() {
    local sdk="$ROOT/../packdb-sdk"
    local crate
    for crate in packdb-client packdb-otel; do
        [[ -f "$sdk/crates/$crate/Cargo.toml" ]] || die "$(
            cat <<EOF
the sibling SDK checkout is missing: expected $sdk/crates/$crate/Cargo.toml

This repository's [patch.crates-io] resolves packdb-client and packdb-otel from
a sibling packdb-sdk checkout, because the CLI depends on them by version and
they are not published yet. Lay the two out side by side:

    <parent>/packdb-sdk/     # the SDK export, at a matching commit
    <parent>/packdb-cli/     # this repository

Once the SDK is published, delete the [patch.crates-io] section from Cargo.toml
and this check goes with it.
EOF
        )"
    done
    log "sibling SDK checkout: $(cd "$sdk" && git rev-parse HEAD 2>/dev/null || echo 'not a git checkout')"
}

# `packdb` is a client: it must never be able to start a server, so it may not
# depend on one. Every PackDB package in its normal and build dependency closure,
# with every feature enabled, must be this package or the SDK it builds on. A
# server package -- `packdb` or any other `packdb-*` crate -- fails the gate.
readonly CLIENT_PACKDB_PACKAGES="packdb-cli packdb-client packdb-otel"
check_client_dependencies() {
    log "client dependency closure"
    local closure unexpected
    closure="$(cargo tree --locked -p packdb-cli --all-features -e normal,build \
        --prefix none --format '{p}')" || die "cargo tree failed for packdb-cli"
    unexpected="$(awk '{print $1}' <<<"$closure" | sort -u |
        grep -E '^packdb($|-)' | grep -vxF -f <(tr ' ' '\n' <<<"$CLIENT_PACKDB_PACKAGES") || true)"
    [[ -z "$unexpected" ]] || die "packdb-cli depends on a server package: $unexpected"
}

check_policy() {
    log "policy files and contacts"
    local f
    for f in LICENSE README.md SECURITY.md CODE_OF_CONDUCT.md CONTRIBUTING.md GOVERNANCE.md \
        SUPPORT.md AI_POLICY.md CODING_STANDARDS.md RELEASING.md MAINTAINERS.md \
        "$CONTRACT" CHANGELOG.md EXPORT_MANIFEST.md docs/WORKFLOWS.md .github/CODEOWNERS; do
        [[ -s "$f" ]] || die "missing or empty policy file: $f"
    done
    # The contract belongs to the crate. A root copy would be a second one that
    # consumers never receive and nothing tests.
    [[ ! -e COMPATIBILITY.md ]] ||
        die "COMPATIBILITY.md exists at the repository root; the contract ships in $CONTRACT"
    if grep -rnE '\{\{[A-Z_]+\}\}' --include='*.md' .; then
        die "unfilled template placeholder"
    fi
    grep -q 'security@packdb.io' SECURITY.md || die "SECURITY.md has no private contact"
    grep -q 'conduct@packdb.io' CODE_OF_CONDUCT.md || die "CODE_OF_CONDUCT.md has no contact"
    grep -q 'Apache License' LICENSE || die "LICENSE is not the Apache License"
}

# POSIX classes, not `\s`: BSD grep and sed do not understand `\s`, so on
# macOS the extraction silently did not strip the `uses:` prefix. The old
# pattern `^[^@]+@[0-9a-f]{40}$` then matched the UNSTRIPPED line anyway --
# `[^@]+` happily absorbs "  - uses: actions/checkout" -- so the gate passed
# for the wrong reason instead of failing loudly. The owner pattern below
# rejects anything carrying whitespace, so a broken extraction now fails.
check_action_pins() {
    log "workflow action pins and triggers"
    local bad
    bad="$(grep -rhoE '^[[:space:]]*(- )?uses:[[:space:]]*[^ #]+' .github/workflows |
        sed -E 's/^[[:space:]]*(- )?uses:[[:space:]]*//' |
        grep -vE '^[A-Za-z0-9._-]+/[A-Za-z0-9._/-]+@[0-9a-f]{40}$' || true)"
    [[ -z "$bad" ]] || die "actions not pinned to a full commit SHA: $bad"
    if grep -rnE 'pull_request_target|workflow_run' .github/workflows; then
        die "privileged workflow trigger is not allowed"
    fi
}

check_packages() {
    log "package metadata and contents"
    python3 - "$REPOSITORY_URL" "${PACKAGES[@]}" <<'PY'
import pathlib, sys, tomllib
repo, *packages = sys.argv[1:]
for package in packages:
    path = pathlib.Path("crates") / package / "Cargo.toml"
    data = tomllib.loads(path.read_text())["package"]
    if data.get("publish") is not False:
        raise SystemExit(f"{package}: publish must remain false")
    if data.get("repository") != repo:
        raise SystemExit(f"{package}: repository must be {repo}")
    for key in ("description", "license", "readme", "keywords"):
        if not data.get(key):
            raise SystemExit(f"{package}: missing package metadata {key}")
PY
    local package listed
    for package in "${PACKAGES[@]}"; do
        listed="$(cargo package --list --locked --allow-dirty -p "$package" 2>/dev/null)" ||
            die "cargo package --list failed for $package"
        # The library and the binary -- and the contract must travel inside the archive,
        # not merely exist in the repo.
        for required in Cargo.toml README.md LICENSE COMPATIBILITY.md src/lib.rs src/main.rs; do
            grep -qx "$required" <<<"$listed" || die "$package package is missing $required"
        done
        if grep -E '(^|/)(target|dist)/|\.profraw$' <<<"$listed"; then
            die "$package package includes a build artifact"
        fi
    done
}

# The workspace has one package, `packdb-cli`: the command library and the
# `packdb` binary, whose process tests check stdout, stderr and exit codes.
# `--all-targets` runs both; `--doc` covers the library's documentation
# examples.
feature_matrix() {
    local toolchain="$1"
    cargo "+$toolchain" test --locked --workspace --all-targets
    cargo "+$toolchain" test --locked --workspace --all-targets --features flight-sql
    cargo "+$toolchain" test --locked --workspace --doc --all-features
}

cmd_checks() {
    check_gate_input checks
    local s changed
    s="$(scope)"
    changed="$(mktemp)"
    changed_files "$changed"
    check_policy
    check_action_pins
    python3 scripts/check_action_pins.py --self-test
    python3 scripts/check_source_headers.py
    require_sdk_sibling
    check_client_dependencies
    python3 scripts/export_clean.py
    python3 scripts/coverage_gate.py --self-test
    cargo fmt --all -- --check
    RUSTFLAGS="-D warnings" cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
    RUSTDOCFLAGS="-D warnings" cargo doc --locked --workspace --all-features --no-deps
    feature_matrix "$MSRV"
    if [[ "$s" == full ]] || grep -qE '^(rust-toolchain\.toml|Cargo\.(toml|lock)|crates/[^/]+/Cargo\.toml)$' "$changed"; then
        rustup toolchain install stable --profile minimal >/dev/null
        feature_matrix stable
    fi
    check_packages
}

cmd_coverage() {
    check_gate_input coverage
    scope >/dev/null
    require_sdk_sibling
    python3 scripts/coverage_gate.py --self-test
    local changed lcov
    changed="$(mktemp)"
    lcov="$(mktemp)"
    changed_files "$changed"
    rustup component add llvm-tools-preview >/dev/null 2>&1 || rustup component add llvm-tools >/dev/null
    install_cargo_tool cargo-llvm-cov "$CARGO_LLVM_COV_VERSION" cargo-llvm-cov
    cargo llvm-cov --locked --workspace --all-features --all-targets --lcov --output-path "$lcov"
    python3 scripts/coverage_gate.py --lcov "$lcov" --changed-files "$changed" --repo-root "$ROOT"
}

cmd_compatibility() {
    check_gate_input compatibility
    scope >/dev/null
    require_sdk_sibling
    # The command contract is checked through the `packdb` binary: its process
    # tests assert stdout, stderr and exit codes.
    feature_matrix "$MSRV"
}

cmd_security() {
    local s
    s="$(scope)"
    require_sdk_sibling
    install_cargo_tool cargo-audit "$CARGO_AUDIT_VERSION" cargo-audit
    install_cargo_tool cargo-deny "$CARGO_DENY_VERSION" cargo-deny
    cargo audit --deny warnings
    cargo deny --locked --all-features check licenses bans sources
    if ! command -v gitleaks >/dev/null 2>&1; then
        [[ -n "${CI:-}" ]] || die "gitleaks $GITLEAKS_VERSION is required"
        require_tool go
        GOBIN="$HOME/.local/bin" go install "github.com/zricethezav/gitleaks/v8@${GITLEAKS_VERSION}"
        export PATH="$HOME/.local/bin:$PATH"
    fi
    local installed
    installed="$(scripts/gitleaks-version.sh "$(command -v gitleaks)")"
    [[ "$installed" == "$GITLEAKS_VERSION" ]] ||
        die "gitleaks is '${installed:-unknown}', expected $GITLEAKS_VERSION"
    if [[ "$s" == full ]]; then
        gitleaks git --no-banner --redact .
    else
        local base="${OSS_BASE_SHA:-}" head="${OSS_HEAD_SHA:-}"
        resolve_commit OSS_BASE_SHA "$base"
        resolve_commit OSS_HEAD_SHA "$head"
        gitleaks git --no-banner --redact --log-opts="${base}..${head}" .
    fi
    python3 scripts/export_clean.py
}

cmd_candidate() {
    local source_sha="${OSS_SOURCE_SHA:-}"
    is_sha "$source_sha" || die "OSS_SOURCE_SHA must be the workflow commit"
    [[ "$(git rev-parse HEAD)" == "$source_sha" ]] || die "checkout is not OSS_SOURCE_SHA"
    git diff --quiet HEAD || die "working tree differs from $source_sha"
    [[ "$(scope)" == full ]] || die "candidate requires OSS_CHECK_SCOPE=full"

    OSS_GATE='' cmd_checks
    OSS_GATE='' cmd_coverage
    OSS_GATE='' cmd_compatibility
    cmd_security

    local version run_id="${GITHUB_RUN_ID:-local}" run_attempt="${GITHUB_RUN_ATTEMPT:-0}"
    version="$(python3 - <<'PY'
import pathlib, tomllib
versions = {
    tomllib.loads(p.read_text())["package"]["version"]
    for p in pathlib.Path("crates").glob("*/Cargo.toml")
}
if len(versions) != 1:
    raise SystemExit(f"candidate requires one reviewed workspace version, got {sorted(versions)}")
print(versions.pop())
PY
)"
    rm -rf dist
    mkdir -p dist/package-inventories
    git archive --format=tar.gz --prefix="packdb-cli-${version}/" \
        --output="dist/packdb-cli-${version}-source.tar.gz" HEAD
    local package
    for package in "${PACKAGES[@]}"; do
        cargo package --list --locked --allow-dirty -p "$package" \
            >"dist/package-inventories/${package}.txt"
    done
    git ls-files >dist/source-inventory.txt
    # The sibling SDK is an input to this library. Recording its commit and
    # inventory digest is what lets a reader confirm afterwards which SDK a
    # candidate was built against, rather than inferring it from timing.
    python3 - "$ROOT/Cargo.lock" >dist/dependencies.json <<'PY'
import json, sys, tomllib
lock = tomllib.load(open(sys.argv[1], "rb"))
print(json.dumps([
    {k: p.get(k) for k in ("name", "version", "source", "checksum")}
    for p in lock.get("package", [])
], indent=2))
PY
    # The SDK checkout the library was built against, by commit and by git tree
    # id -- a content digest of exactly those sources. No fallback: a candidate
    # that cannot name its SDK must not be produced.
    local sdk_sha sdk_tree
    sdk_sha="$(git -C "$ROOT/../packdb-sdk" rev-parse HEAD)"
    sdk_tree="$(git -C "$ROOT/../packdb-sdk" rev-parse 'HEAD^{tree}')"
    local sums
    sums="$(mktemp)"
    (cd dist && find . -type f ! -name SHA256SUMS -print0 | sort -z |
        xargs -0 shasum -a 256 >"$sums")
    mv "$sums" dist/SHA256SUMS
    python3 - "$source_sha" "$version" "$run_id" "$run_attempt" "$sdk_sha" "$sdk_tree" \
        >dist/release-record.json <<'PY'
import json, subprocess, sys
sha, version, run_id, attempt, sdk_sha, sdk_tree = sys.argv[1:7]
print(json.dumps({
    "repository": "packdb-cli",
    "packages": ["packdb-cli"],
    "version": version,
    "source_sha": sha,
    "sdk_source_sha": sdk_sha,
    "sdk_tree": sdk_tree,
    "toolchain": subprocess.run(["rustc", "--version"], capture_output=True, text=True, check=True).stdout.strip(),
    "gates": ["checks", "coverage", "compatibility", "security"],
    "scope": "full",
    "publication_enabled": False,
    "candidate_run_id": run_id,
    "candidate_run_attempt": attempt,
    "artifact_name": f"candidate-{sha}-{run_id}-{attempt}",
    "checksums": open("dist/SHA256SUMS").read().splitlines(),
}, indent=2))
PY
    (cd dist && shasum -a 256 release-record.json >>SHA256SUMS)
}

cmd_publish() {
    die "publication is not enabled for this repository; see RELEASING.md"
}

case "${1:-}" in
    checks) cmd_checks ;;
    coverage) cmd_coverage ;;
    compatibility) cmd_compatibility ;;
    security) cmd_security ;;
    candidate) cmd_candidate ;;
    publish) cmd_publish ;;
    *) die "usage: scripts/oss-ci.sh checks|coverage|compatibility|security|candidate|publish" ;;
esac
