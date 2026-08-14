#!/usr/bin/env bash
# Materialise govox-py's source at the pinned commit, for parity generation.
#
# Extracts with `git archive`, which is a pure read: it does not create a git
# worktree, touch the index, move HEAD, or write anything under govox-py/.git.
# That matters because govox-py is under active development by someone else and
# is the reference implementation — this port must never perturb it.
#
# Usage:
#   ./pinned-source.sh [dest]        # default: .parity-src/ beside this script
#
# Environment:
#   GOVOX_PY_REPO   path to the govox-py checkout (default: ../govox-py)
#
# Output: a tree containing the pinned `src/` and `config/`, plus a STAMP file
# recording which commit it came from.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../.." && pwd)"

# shellcheck disable=SC1091
source "$root/REFERENCE"

# Resolve the sibling checkout relative to the *main* working tree, not to
# $root: inside a git worktree (.claude/worktrees/<name>) the parent directory
# is the worktree pool, not the playground, so "../govox-py" would miss.
main_root="$root"
if common_dir="$(git -C "$root" rev-parse --path-format=absolute --git-common-dir 2>/dev/null)"; then
  main_root="$(dirname "$common_dir")"
fi

if [ -n "${GOVOX_PY_REPO:-}" ]; then
  repo="$GOVOX_PY_REPO"
else
  repo="$(cd "$main_root/.." && pwd)/$(basename "$GOVOX_PY_REPO_DEFAULT")"
fi
dest="${1:-$here/.parity-src}"

if [ ! -d "$repo/.git" ]; then
  echo "error: no govox-py checkout at $repo (set GOVOX_PY_REPO)" >&2
  exit 1
fi

if ! git -C "$repo" cat-file -e "${GOVOX_PY_COMMIT}^{commit}" 2>/dev/null; then
  echo "error: $repo does not contain pinned commit $GOVOX_PY_COMMIT" >&2
  echo "hint: it may need fetching, or REFERENCE may name a commit from another repo" >&2
  exit 1
fi

# Already materialised at the right commit? Nothing to do.
if [ -f "$dest/STAMP" ] && grep -qx "$GOVOX_PY_COMMIT" "$dest/STAMP"; then
  echo "pinned source already present at $dest ($GOVOX_PY_COMMIT)"
  exit 0
fi

rm -rf "$dest"
mkdir -p "$dest"

# Only what the generators actually import. Keeping the extract narrow means a
# change anywhere else in govox-py cannot silently alter a corpus run.
git -C "$repo" archive "$GOVOX_PY_COMMIT" src config | tar -x -C "$dest"

echo "$GOVOX_PY_COMMIT" > "$dest/STAMP"
echo "extracted govox-py@${GOVOX_PY_COMMIT:0:12} -> $dest"
echo
echo "run generators against it with:"
echo "  PYTHONPATH=$dest/src <interpreter> <generator.py>"
