#!/usr/bin/env bash
# Run one real agent task with Mush as its only shell and file layer.
#
# This is the end-to-end check the harness cannot be: a model, the Pi runtime,
# the extension's tool swap, and the daemon, all in one loop. The task is small
# and its result is inspectable in the VFS, so the test says whether the agent
# finished inside the sandbox instead of guessing from its prose.
#
# Usage: scripts/mush-agent-test.sh [--keep]
# Env: PI_BIN, MUSH_BIN, AGENT_MODEL (passed through as --model), MUSH_TIMEOUT
set -uo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
mush="${MUSH_BIN:-$root/target/debug/mush}"
pi_bin="${PI_BIN:-$(command -v pi || true)}"
[ -n "$pi_bin" ] || pi_bin="$HOME/.pi/agent/bin/pi"
keep=0
[ "${1:-}" = "--keep" ] && keep=1

for bin in "$mush" "$pi_bin"; do
  if [ ! -x "$bin" ]; then
    echo "missing $bin" >&2
    exit 2
  fi
done

work="$(mktemp -d "${TMPDIR:-/tmp}/mush-agent.XXXXXX")"
vfs="$work/vfs"
cleanup() {
  if [ "$keep" -eq 1 ]; then
    echo "kept $work" >&2
  else
    rm -rf "$work"
  fi
}
trap cleanup EXIT

seed() {
  "$mush" --root "$vfs" sh -c "$1" >/dev/null || {
    echo "seed failed: $1" >&2
    exit 1
  }
}
seed 'mkdir /project'
seed 'write /project/config.json "{\"name\":\"demo\",\"features\":[]}"'
# Single quotes reach the Spore's tokenizer intact, so `\n` is a newline here.
seed "printf 'start\\ndone\\n' > /project/notes.md"

prompt="$(
  cat <<'TEXT'
Work only through the mush and vfs_* tools. This session is sandboxed: the
working directory is `/` in a VFS, not the host checkout, and bash/read/write
are not available.

In /project:
1. Set the JSON field `name` to "mush".
2. Add "diff" as the next element of the `features` array.
3. Append a line `reviewed by agent` to /project/notes.md.
4. Create /project/status.txt containing exactly the line: ready

Then reply with one line: what /project/config.json now contains.
TEXT
)"

echo "== agent ==" >&2
model_args=()
[ -n "${AGENT_MODEL:-}" ] && model_args=(--model "$AGENT_MODEL")
MUSH_ROOT="$vfs" MUSH_BIN="$mush" "${pi_bin}" -p --no-session \
  --no-builtin-tools --no-extensions --no-mcp --no-skills \
  --extension "$root/integrations/pi/mush.ts" \
  "${model_args[@]}" \
  --tools 'mush,vfs_read,vfs_write,vfs_edit,vfs_grep,vfs_find,vfs_ls,vfs_diff,vfs_json' \
  --append-system-prompt "The shell and file tools are Mush's; paths are VFS-absolute." \
  "$prompt"
status=$?
echo "== agent exit $status ==" >&2

config="$("$mush" --root "$vfs" sh -c 'cat /project/config.json')"
notes="$("$mush" --root "$vfs" sh -c 'cat /project/notes.md')"
status_file="$("$mush" --root "$vfs" sh -c 'cat /project/status.txt' 2>/dev/null)"

python3 - "$config" "$notes" "$status_file" <<'PY'
import json, sys
config, notes, status = sys.argv[1], sys.argv[2], sys.argv[3]
failures = []
try:
    doc = json.loads(config)
except json.JSONDecodeError as error:
    failures.append(f"config.json is not JSON: {error}")
    doc = {}
if doc.get("name") != "mush":
    failures.append(f"name is {doc.get('name')!r}, want 'mush'")
if doc.get("features") != ["diff"]:
    failures.append(f"features is {doc.get('features')!r}, want ['diff']")
if "reviewed by agent" not in notes:
    failures.append(f"notes.md lost the appended line: {notes!r}")
if status.strip() != "ready":
    failures.append(f"status.txt is {status!r}, want 'ready'")
if failures:
    print("\n".join(f"FAIL {failure}" for failure in failures), file=sys.stderr)
    sys.exit(1)
print("ok: agent finished the task inside the VFS")
PY
check=$?
[ "$status" -ne 0 ] && [ "$check" -eq 0 ] && { echo "agent exited $status" >&2; exit 1; }
exit "$check"
