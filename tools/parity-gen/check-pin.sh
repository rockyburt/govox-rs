#!/usr/bin/env bash
# Assert REFERENCE names a full 40-character commit SHA.
#
# A branch name or an abbreviated SHA would let the parity corpus re-baseline
# itself against upstream work in progress, which turns a genuine divergence
# into a passing test. Run by CI; also useful locally after moving the pin.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# shellcheck disable=SC1091
source "$root/REFERENCE"

if [ -z "${GOVOX_PY_COMMIT:-}" ]; then
  echo "error: REFERENCE does not set GOVOX_PY_COMMIT" >&2
  exit 1
fi

if ! printf '%s' "$GOVOX_PY_COMMIT" | grep -Eqx '[0-9a-f]{40}'; then
  echo "error: GOVOX_PY_COMMIT must be a full 40-char SHA, got '$GOVOX_PY_COMMIT'" >&2
  exit 1
fi

echo "reference pin OK: $GOVOX_PY_COMMIT (${GOVOX_PY_DATE:-date unknown})"
