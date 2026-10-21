#!/usr/bin/env bash
# Copyright 2025 PackDB Contributors
# Licensed under the Apache License, Version 2.0
#
# Assert COMPATIBILITY.md against a BUILT binary.
#
#     CONTRACT=<path> scripts/assert-shipped-contract.sh <binary>
#
# One definition, two callers: the `compatibility` gate runs it against a
# freshly built binary, and the candidate workflow runs it against the binary
# extracted from the archive it is about to publish. If those two could drift,
# the gate would pass while the shipped artifact broke the contract.
set -euo pipefail

CONTRACT="${CONTRACT:-crates/packdb-query-cli/COMPATIBILITY.md}"
BIN="${1:-}"

die() { echo "contract: $*" >&2; exit 1; }

[[ -n "$BIN" ]] || die "usage: CONTRACT=<path> $0 <binary>"
[[ -x "$BIN" ]] || die "not an executable binary: $BIN"
[[ -s "$CONTRACT" ]] || die "contract is missing or empty: $CONTRACT"

"$BIN" --version >/dev/null
"$BIN" --help >/dev/null

# Released binaries enable flight-sql on every target, so both transport flags
# are always present. A build without them presents a different flag surface,
# which is exactly the divergence the contract exists to prevent.
"$BIN" --help | grep -q -- --flight-endpoint || die "--flight-endpoint is not advertised"
"$BIN" --help | grep -q -- --http-only || die "--http-only is not advertised"

check() {
    local want="$1"; shift
    local got
    set +e; "$BIN" "$@" >/dev/null 2>&1; got=$?; set -e
    [[ "$got" == "$want" ]] || die "exit $got, contract says $want, for: $*"
}
check 0 --help
check 0 --version
check 2
check 2 --format bogus "SELECT 1"
check 2 --format JSON "SELECT 1"
check 2 --url http://127.0.0.1:9 --timeout 1 "SELECT 1"

# The release matrix in the shipped contract must be the matrix that is
# actually built. A withdrawn target reappearing is a promise nobody made.
grep -q 'x86_64-unknown-linux-gnu' "$CONTRACT" || die "contract lost the Linux target"
grep -q 'aarch64-apple-darwin' "$CONTRACT" || die "contract lost the macOS target"
if grep -qE '^\| `(aarch64-unknown-linux-gnu|x86_64-apple-darwin)` \|' "$CONTRACT"; then
    die "the contract's release matrix lists a withdrawn target"
fi

echo "contract holds: transports, all documented exit codes, release matrix"
