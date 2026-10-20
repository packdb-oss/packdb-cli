#!/usr/bin/env bash
# Copyright 2026 PackDB Contributors
#
# Licensed under the Apache License, Version 2.0
set -euo pipefail

# Release binaries print their version directly. Binaries installed with
# `go install` may instead print "version is set by build process"; their
# module version remains available in Go's embedded build metadata.
binary="${1:-}"
[[ -n "$binary" ]] || {
    echo "usage: scripts/gitleaks-version.sh /path/to/gitleaks" >&2
    exit 2
}

installed=""
if command -v go >/dev/null 2>&1; then
    installed="$(go version -m "$binary" 2>/dev/null |
        awk '$1 == "mod" { print $3; exit }' || true)"
fi
if [[ -z "$installed" ]]; then
    installed="v$("$binary" version 2>/dev/null | sed -E 's/^v//')"
fi
printf '%s\n' "$installed"
