#!/bin/sh
# Seed $AGENTJAIL_STATE_DIR/flavors/ with the runtimes baked into the
# server image (nodejs, python, bun). Idempotent: only creates a flavor
# directory when it's missing, so operators can add their own
# custom / versioned flavors without this script stomping them.
#
# Why symlinks into /usr/bin and not copies: the jail already
# bind-mounts /usr/bin, /bin, /lib, /lib64 from the host rootfs, so
# /opt/flavors/<name>/bin/<tool> resolves transparently to the real
# binary via the symlink. Copying the binaries would just duplicate
# bytes + break dynamic-library resolution.
#
# Called from Dockerfile.server's ENTRYPOINT wrapper.

set -eu

ROOT="${AGENTJAIL_STATE_DIR:-/var/lib/agentjail}/flavors"
mkdir -p "$ROOT"

seed_flavor() {
    name="$1"
    shift
    # Each remaining arg is a `binary:target` pair.
    dir="$ROOT/$name"
    if [ -e "$dir" ]; then
        return 0
    fi
    mkdir -p "$dir/bin"
    for pair in "$@"; do
        bin="${pair%%:*}"
        target="${pair#*:}"
        # Skip the flavor entirely if the backing binary isn't
        # present — happens when a build removes a package but the
        # seed list wasn't updated. Empty flavor dirs get cleaned up
        # so they don't mislead the UI.
        if [ ! -e "$target" ]; then
            rm -rf "$dir"
            echo "seed-flavors: $name skipped (missing $target)" >&2
            return 0
        fi
        ln -sf "$target" "$dir/bin/$bin"
    done
    echo "seed-flavors: seeded $name -> $dir" >&2
}

seed_flavor nodejs node:/usr/bin/node npm:/usr/bin/npm npx:/usr/bin/npx
seed_flavor python python3:/usr/bin/python3 pip:/usr/bin/pip3
seed_flavor bun    bun:/usr/local/bin/bun
