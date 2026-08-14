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

echo "==> building $(git rev-parse --abbrev-ref HEAD) in $REPO"
# Only the binary. Building the whole workspace would compile the test targets
# too, which roughly doubles the cycle for code that is not about to run.
cargo build -p govox --bin govox

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
