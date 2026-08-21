#!/usr/bin/env bash
#
# Build the current checkout and swap the running daemon onto it.
#
# The edit-run cycle for a daemon that owns the microphone, the keyboard and an
# input method is otherwise fiddly: kill it, rebuild, restart, remember which
# worktree the binary came from. This is that, in one command, and it is the
# command to use rather than `systemctl --user restart govox-rs-dev` on its own
# — restarting without building relaunches the *old* binary, which looks
# exactly like a change that did not work.
#
#   tools/dev-restart.sh            # build, restart, show the last log lines
#   tools/dev-restart.sh --follow   # ...and then tail the journal
#
# Run it from any worktree. The build writes to the shared target directory the
# unit points at, so whichever checkout you build from is the one that runs.

set -euo pipefail

UNIT=govox-rs-dev.service
REPO=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)

cd "$REPO"

# Asked for rather than assumed: `.cargo/config.toml` redirects the target
# directory away from the checkout, which is the whole reason a build from one
# worktree can change what another one runs — and the reason the version check
# below exists at all.
TARGET_DIR=$(cargo metadata --format-version 1 --no-deps --offline 2>/dev/null \
    | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')
if [[ -z "$TARGET_DIR" ]]; then
    echo "==> could not determine the target directory" >&2
    exit 1
fi

echo "==> building $(git rev-parse --abbrev-ref HEAD) in $REPO"
# Only the binary. Building the whole workspace would compile the test targets
# too, which roughly doubles the cycle for code that is not about to run.
cargo build -p govox --bin govox

# The version string is baked in by govox-daemon's build script, and cargo only
# re-runs that when it thinks one of the git refs it watches has changed. That
# judgement has been observed going wrong: a shared target directory built from
# two checkouts left a stale build-script unit feeding the binary, and
# `--version` reported a commit and a release a day behind the tree. A wrong
# answer here is worse than no answer, because this string exists to be asked
# during a restart — exactly when a stale one is believed.
#
# So it is checked rather than trusted, against the one fact that goes stale:
# the commit. Deliberately not by recomputing the whole string in bash. A second
# implementation of the version format, agreeing in the common case and
# diverging on tags, would be the same class of bug one layer up.
BIN="$TARGET_DIR/debug/govox"
COMMIT=$(git rev-parse --short=7 HEAD)
ON_TAG=$(git describe --tags --exact-match HEAD 2>/dev/null || true)
REPORTED=$("$BIN" --version 2>/dev/null || echo "unknown")

version_is_current() {
    if [[ -n "$ON_TAG" ]]; then
        # A release wears its bare version number, with no build metadata.
        [[ "$REPORTED" != *"+"* ]]
    else
        [[ "$REPORTED" == *"$COMMIT"* ]]
    fi
}

if ! version_is_current; then
    echo "==> $REPORTED does not name $COMMIT; rebuilding the version"
    # One crate, a few seconds, and only once the check has already failed.
    # This is the smallest hammer that reliably re-runs the build script.
    cargo clean -p govox-daemon
    cargo build -p govox --bin govox
    REPORTED=$("$BIN" --version 2>/dev/null || echo "unknown")
    if ! version_is_current; then
        echo "==> the version is still stale: $REPORTED, expected $COMMIT" >&2
        echo "==> refusing to restart onto a binary that cannot say what it is" >&2
        exit 1
    fi
fi
echo "==> built $REPORTED"

echo "==> restarting $UNIT"
systemctl --user restart "$UNIT"

# Give it long enough to fail properly. A config error exits 2 almost at once,
# and reporting "started" for a unit that is already dead is the one outcome
# this script must not produce.
sleep 2
if ! systemctl --user is-active --quiet "$UNIT"; then
    echo "==> $UNIT is NOT running:" >&2
    systemctl --user status --no-pager --lines=20 "$UNIT" >&2 || true
    exit 1
fi

echo "==> running"
if [[ "${1:-}" == "--follow" ]]; then
    exec journalctl --user -u "$UNIT" -f
fi
journalctl --user -u "$UNIT" --no-pager --lines=15 --since "10 seconds ago"
