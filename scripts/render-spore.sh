#!/bin/sh
# Render a Spore package to a frame with the one renderer.
#
#   render-spore.sh <package> <region|-> <width> <height> <out.rgba> [snapshot.json]
#
# Two steps, one renderer: `soil-lua` runs the package and emits the document a
# host presents; `soil-scene-gpu` rasterizes that document. A native window host,
# Canvas and the browser all sit downstream of the same two steps — the browser
# just supplies a surface instead of a file.
set -eu
[ $# -ge 5 ] || { echo "usage: $0 <package> <region|-> <width> <height> <out.rgba> [snapshot.json]" >&2; exit 2; }
package=$1; region=$2; width=$3; height=$4; out=$5; snapshot=${6:-}

set -- run "$package"
[ "$region" != "-" ] && set -- "$@" --region "$region"
set -- "$@" --width "$width" --height "$height"
[ -n "$snapshot" ] && set -- "$@" --snapshot "$snapshot"

scene=$(mktemp -t spore-scene.XXXXXX.json)
trap 'rm -f "$scene"' EXIT

cargo run -q -p soil-lua -- "$@" --out "$scene"
cargo run -q -p soil-scene-gpu --bin soil-scene-gpu -- --scene "$scene" --size "${width}x${height}" --rgba "$out"
