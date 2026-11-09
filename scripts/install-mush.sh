#!/bin/sh
# Build mush and install it over the running binary.
#
# Two environment hazards this exists to avoid:
#   - a RUSTC_WRAPPER pointing at a dead sccache daemon blocks every rustc
#     invocation until its startup timeout, which looks like a hung build.
#   - a deleted TMPDIR (a per-turn scratch directory) makes the daemon fail to
#     start at all.
# Both are stripped here rather than documented and forgotten.
#
# CARGO_HOME is inherited, not invented. The default ~/.cargo is shared with
# every other agent on the machine, and a build there can sit on the package
# cache lock until the other build finishes; `fpl bob cargo-home --export` gives
# this shell its own lock while sharing the registry content.
set -eu

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
profile=${PROFILE:-debug}
prefix=${PREFIX:-$HOME/.local/bin}
share=${MUSH_SHARE:-$HOME/.local/share/mush}

cd "$root"
unset RUSTC_WRAPPER SCCACHE_CONF SCCACHE_BASEDIRS
export TMPDIR=${TMPDIR:-/tmp}

if [ "$profile" = release ]; then
    cargo build -p mush --release --offline
    built=$root/target/release/mush
else
    cargo build -p mush --offline
    built=$root/target/debug/mush
fi

mkdir -p "$prefix" "$share"
# Install through a temporary name so a running agent never sees a half-written
# binary at the path it is about to exec.
install -m 755 "$built" "$prefix/.mush.new"
mv -f "$prefix/.mush.new" "$prefix/mush"
# The component travels with the binary: an installed mush is not next to a
# checkout, and the binary looks here when spores/mush/module.wasm is absent.
install -m 644 "$root/spores/mush/module.wasm" "$share/module.wasm"
printf 'installed %s (%s) and %s/module.wasm\n' "$prefix/mush" "$profile" "$share"
