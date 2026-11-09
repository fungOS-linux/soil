#!/usr/bin/env bash
# Measure how much of an agent's shell work Mush can actually do.
#
# The question this answers is not "is Mush a shell" -- it is -- it is "can an
# agent swap its shell for Mush and still finish its job". An agent's shell
# spends most of its time on the repository toolchain, so the probe runs the
# same command lines through both shells over equivalent content and counts
# completions per class.
#
# Usage: scripts/mush-agent-parity.sh [--mush PATH] [--shell PATH] [--keep]
set -uo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
mush="$root/target/debug/mush"
shell="${SHELL:-/bin/sh}"
keep=0
while [ $# -gt 0 ]; do
  case "$1" in
    --mush) mush="$2"; shift 2 ;;
    --shell) shell="$2"; shift 2 ;;
    --keep) keep=1; shift ;;
    *) echo "unknown option $1" >&2; exit 2 ;;
  esac
done
if [ ! -x "$mush" ]; then
  echo "no mush at $mush; build it with: cargo build -p mush" >&2
  exit 2
fi

work="$(mktemp -d "${TMPDIR:-/tmp}/mush-parity.XXXXXX")"
cleanup() {
  if [ "$keep" -eq 1 ]; then
    echo "fixtures kept at $work" >&2
  else
    rm -rf "$work"
  fi
}
trap cleanup EXIT

# Equivalent content on both sides: two files at the same relative paths.
host="$work/host"
vfs="$work/vfs"
mkdir -p "$host/src" "$vfs"
printf 'hello parity\n' > "$host/README.md"
printf 'fn hello\n' > "$host/src/main.rs"
(
  cd "$host" || exit 1
  git init -q
  git -c user.email=probe@example.invalid -c user.name=probe add -A
  git -c user.email=probe@example.invalid -c user.name=probe commit -qm init
)
"$mush" --root "$vfs" --timeout 20 --command \
  'mkdir src
write README.md hello parity
write src/main.rs fn hello
exit' >/dev/null 2>&1

# A command that never returns must not stall the probe itself.
bounded() {
  local secs="$1"; shift
  "$@" &
  local pid=$!
  ( sleep "$secs"; kill -9 "$pid" 2>/dev/null ) >/dev/null 2>&1 &
  local watcher=$!
  wait "$pid" 2>/dev/null
  local rc=$?
  kill "$watcher" 2>/dev/null
  return "$rc"
}

run_host() {
  bounded 30 "$shell" -c "cd '$host' && $1" >/dev/null 2>&1
}
run_mush() {
  bounded 40 "$mush" --root "$vfs" --timeout 30 sh -c "$1" </dev/null >/dev/null 2>&1
}

vfs_commands=(
  'pwd'
  'ls'
  'ls src'
  'cat README.md'
  'grep hello src/main.rs'
  'wc -l README.md'
  'mkdir out'
  'echo hi > note.txt'
)
toolchain_commands=(
  'git --version'
  'git log --oneline'
  'cargo --version'
  'rg --version'
  'python3 --version'
  'sed -n 1p README.md'
)

pass_host=0
pass_mush=0
report() {
  local class="$1"; shift
  local total=0
  local ok_host=0
  local ok_mush=0
  printf '\n%s\n' "$class"
  printf '  %-26s %-6s %-6s\n' 'command' "$(basename "$shell")" mush
  for command in "$@"; do
    total=$((total + 1))
    local host_mark mush_mark
    if run_host "$command"; then
      ok_host=$((ok_host + 1))
      host_mark=ok
    else
      host_mark=FAIL
    fi
    if run_mush "$command"; then
      ok_mush=$((ok_mush + 1))
      mush_mark=ok
    else
      mush_mark=FAIL
    fi
    printf '  %-26s %-6s %-6s\n' "$command" "$host_mark" "$mush_mark"
  done
  printf '  %-26s %-6s %-6s\n' 'class total' "$ok_host/$total" "$ok_mush/$total"
  pass_host=$((pass_host + ok_host))
  pass_mush=$((pass_mush + ok_mush))
  total_all=$((total_all + total))
}

total_all=0
report 'VFS work (the shell over a file store)' "${vfs_commands[@]}"
report 'Repository toolchain (the job an agent shell exists for)' "${toolchain_commands[@]}"

printf '\noverall: %s %s/%s, mush %s/%s\n' "$(basename "$shell")" \
  "$pass_host" "$total_all" "$pass_mush" "$total_all"
