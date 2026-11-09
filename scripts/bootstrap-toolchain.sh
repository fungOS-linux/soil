#!/usr/bin/env bash
# Fetch the Zig toolchain `ghostty-vt-sys` needs to build libghostty-vt.
#
# Zig is not an optional convenience here: Ghostty requires an exact minor
# version, and the host's `zig` is frequently something else. `.cargo/config.toml`
# points `ZIG` at this checkout, so building soil-vt does not depend on PATH.
set -euo pipefail

version="0.17.0"
root="$(cd "$(dirname "$0")/.." && pwd)"
dest="$root/.toolchain/zig"

if [[ -x "$dest/zig" ]] && [[ "$("$dest/zig" version)" == "$version" ]]; then
  echo "zig $version already at $dest"
  exit 0
fi

case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) target="aarch64-macos" ;;
  Darwin-x86_64) target="x86_64-macos" ;;
  Linux-x86_64) target="x86_64-linux" ;;
  Linux-aarch64 | Linux-arm64) target="aarch64-linux" ;;
  *)
    echo "unsupported platform: $(uname -s) $(uname -m)" >&2
    echo "install Zig $version and set ZIG to its binary" >&2
    exit 1
    ;;
esac

archive="zig-$target-$version.tar.xz"
url="https://ziglang.org/download/$version/$archive"
mkdir -p "$root/.toolchain"
tmp="$(mktemp -d "$root/.toolchain/zig.XXXXXX")"
trap 'rm -rf "$tmp"' EXIT

echo "fetching $url"
curl -fsSL "$url" -o "$tmp/$archive"
tar -xf "$tmp/$archive" -C "$tmp"
rm -rf "$dest"
mv "$tmp/zig-$target-$version" "$dest"

actual="$("$dest/zig" version)"
if [[ "$actual" != "$version" ]]; then
  echo "expected zig $version, got $actual" >&2
  exit 1
fi
echo "zig $version -> $dest"
