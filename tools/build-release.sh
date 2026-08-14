#!/usr/bin/env bash
# Build the release binaries inside an older distribution than this machine.
#
# The point is the glibc floor, nothing else. Linking against the newest glibc
# on the build host binds the binary to symbol versions that host happens to
# have, and a binary built on Ubuntu 26.04 (glibc 2.43) refuses to start
# anywhere older with `version 'GLIBC_2.43' not found` — before main() runs, and
# with no hint that a different build exists. Ubuntu 24.04 ships glibc 2.39,
# which covers every current LTS and Debian stable.
#
# The container is a build environment only: same source, same toolchain pin,
# same default features. Only the libc the linker sees changes.
#
# Usage: tools/build-release.sh [output-dir]   (default: dist/)
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
out_dir="${1:-$repo_root/dist}"
image="ubuntu:24.04"

mkdir -p "$out_dir"

# /src is read-only so the build cannot mutate the checkout, and CARGO_TARGET_DIR
# points somewhere container-local: the repo's untracked .cargo/config.toml sets
# an absolute host target-dir that does not exist in here, and an env var wins
# over it.
docker run --rm \
    -v "$repo_root":/src:ro \
    -v "$out_dir":/out \
    -e CARGO_TERM_COLOR=never \
    "$image" bash -euxo pipefail -c '
        export DEBIAN_FRONTEND=noninteractive
        apt-get update -qq
        # Two of these are not obvious, and both are invisible on a developer
        # machine and on a GitHub runner because each ships them already:
        #   libssl-dev   arrives via hf-hub -> reqwest -> native-tls, for model
        #                downloads.
        #   libclang-dev is what bindgen loads to generate whisper.cpp bindings.
        # A clean container is the only place their absence shows up, which is
        # most of the value of building this way.
        apt-get install -y --no-install-recommends \
            build-essential cmake pkg-config curl ca-certificates git \
            libasound2-dev libvulkan-dev glslc libssl-dev libclang-dev

        # No toolchain named: rustup reads /src/rust-toolchain.toml, so the
        # container builds with exactly the pinned version CI uses.
        curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs \
            | sh -s -- -y --profile minimal --default-toolchain none
        . "$HOME/.cargo/env"

        cd /src
        export CARGO_TARGET_DIR=/build
        cargo build --release --locked

        install -m 0755 /build/release/govox /build/release/govox-overlay /out/
    '

echo
echo "built into $out_dir:"
ls -lh "$out_dir/govox" "$out_dir/govox-overlay"
