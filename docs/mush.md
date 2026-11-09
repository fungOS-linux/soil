# Mush

Mush is the sandboxed shell over one VFS workspace. The command engine is a
Wasm Spore; the native `mush` process is only the host.

A Mush session is intended to run inside a **Desktop**, the durable top-level
namespace for a body of work. `--root` is currently a standalone VFS root; the
Desktop binding adds project mounts, a records scope, and a stable identity. See
[Desktops](desktop.md).

```sh
mush --root ~/fungos-node
```

The Spore imports four capabilities:

```text
soil:vfs       read, stat, list
soil:index     browse, search, KNN
soil:vfs-mut   write, remove, mkdir, rmdir, rename
soil:command   virtual PATH to installed wasm commands
```

It has no host filesystem, host `PATH`, subprocess, or network access. The
virtual `PATH` resolves only installed wasm command packages.

## Shell state

Mush keeps real shell state across commands:

```text
PWD, OLDPWD, HOME, TERM, PATH, USER, LOGNAME, SHELL, PS1
```

```sh
cd /projects
pwd
echo $PWD
export TERM=xterm-256color
export PS1="\u@mush:\w$ "
cd -
env
```

`PWD` is updated on `cd`; `cd -` returns to `OLDPWD`.

## Startup file

Mush loads `.mushrc` (or `mushrc`) from the VFS root before the first prompt:

```sh
# .mushrc
export TERM=xterm-256color
export PS1="mush:\w$ "
export PROJECT=soil
mkdir notes
```

The file is VFS content, not a host dotfile.

## Install the host shell

The host binary embeds `spores/mush/module.wasm`, so it does not need a sidecar
file at runtime:

```sh
cargo build -p mush --release
install -m755 target/release/mush ~/.local/bin/mush
```

Then:

```sh
mush --root ~/fungos-node
```

This installs the **host** launcher named `mush`. It does not create
`/bin/mush` inside the VFS, and it does not turn the host process into a
container. The Spore remains the sandboxed part.

## Config and doctor

Mush reads `~/.config/fungos/mush/config.toml` unless `--config PATH` or
`MUSH_CONFIG` overrides it. CLI flags win over environment variables, which
win over the config file.

```toml
root = "~/fungos-node"
command_root = "~/.local/share/fungos/commands"
records_root = "~/.local/share/fungos/records"
records_book = "personal/avery/marbles"
records_collection = "marbles"
principal = "mycelium:agent:ajmwagar/codex-1"
socket = "~/.local/share/fungos/mush.sock"
desktop = "acme"
```

```sh
mush doctor
```

`doctor` reports the effective VFS root, command root, records binding, Spore
source, principal, Desktop, and service socket. Missing roots are errors, not
silent fallbacks.

## Desktop

`mush desktop` manages the local registry in `~/.config/fungos/desktops/`:

```sh
mush desktop create acme --name Acme --theme ocean
mush desktop list
mush desktop show acme
mush --desktop acme
```

A Desktop selects a VFS root under `~/.local/share/fungos/desktops/<id>`.
That root can later grow project worktree mounts; today it is a namespace with
`projects/`, `notes/`, and `scratch/` directories. See [Desktops](desktop.md).

A Desktop also records `mount`, where this device projects it for host tools
(default `~/mnt/<id>`). The root is the store; the mount is a filesystem view of
it, and they are separate because a Desktop follows a principal across machines
while each device projects it somewhere different.

### Origin

`mush origin` answers "which Desktop owns this caller?" from the only two hints
a Delta agent has: `DELTA_CURRENT_THREAD_ID` and a working directory shaped like
`<repo>/.delta/worktrees/<id>/<repo>`.

```sh
mush origin                 # human-readable
mush origin --json          # machine-readable
mush origin --cwd PATH      # resolve somewhere else
```

```text
cwd: /src/soil/.delta/worktrees/abc123/soil
thread: ksQQ...
delta worktree: abc123
repo: soil
desktop: personal
vfs root: /Users/me/.local/share/fungos/desktops/personal
mount: /Users/me/mnt/personal [mounted]
project: /Users/me/mnt/personal/projects/soil
status: ready
```

`status` is honest about what is actually there:

| `status` | Meaning | Exit |
|---|---|---|
| `ready` | The projection is mounted and the project exists under it | 0 |
| `not-mounted` | A Desktop matched, but nothing is mounted at its path | 2 |
| `no-project` | The projection is mounted, but this repository is not under it | 2 |
| `unregistered` | No Desktop claims this repository or path | 3 |

Resolution never invents a mount. Handing back a path where nothing is mounted
would send an agent's writes into an empty directory that looks exactly like
success, so `not-mounted` is a distinct, non-zero answer.

When no `--root`, `--desktop`, `MUSH_DESKTOP`, or `desktop` config names a
Desktop, `mush` selects the one its origin resolves to and says so on stderr.
A caller that wants the default root or a specific Desktop passes one
explicitly, which always wins.

## Service mode

`mush --serve` binds a `0600` Unix socket and accepts one JSON command request
per connection. It refuses to replace an existing socket and checks the peer
UID against the service account. See
[service accounts](mush-service-accounts.md) for the `ajmwagar-agents` layout.

Use the client from another process:

```sh
mush request 'mb ready --json'
mush request --json 'pwd'
```

`mush request` prints the command output and exits with the command's shell
status. `--json` returns the full service envelope, including `status`, `exit`
and `output`.

For Operator, use the MCP endpoint:

```sh
mush mcp --foreground --listen 127.0.0.1:0
```

It exposes `mcp__mush__mush_exec` over loopback HTTP. See
[Operator integration](mush-operator.md).

## Deadlines and servers

A command runs inside the Spore, so nothing outside it can see progress. A
command that never returns -- `while true; do :; done`, or `read` on a stdin
that never closes -- would park the caller forever with no output and no exit
status. That is the worst failure available to an agent harness, so it is
prevented by contract rather than by hope.

Every non-interactive command runs with a deadline:

```text
mush --command 'loop'                 default 120s
mush --timeout 600 --command 'mb ...' longer budget
mush --timeout 0 --command 'x'        deadline disabled
mush request --timeout 30 'mb ready'  client bound and daemon budget
```

`MUSH_TIMEOUT` sets the same value in seconds. An interactive session has no
deadline, because there the human is the clock.

The deadline is enforced with Wasmtime epoch interruption, so the Spore is
actually stopped rather than abandoned. The caller sees:

```text
mush: command exceeded 30s and was interrupted
mush: hint: raise it with --timeout SECONDS, or split the work into steps that finish
```

with shell status `124` and, over the service or MCP envelope, `state`
`"timeout"` and `blocking` `true`.

Servers are the other way a Mush invocation can hold a process open. `mush
--serve` and `mush mcp` do not return, so they refuse to start when stdin is not
a terminal unless `--foreground` says a supervisor owns the process. A server
started from a tool call is indistinguishable from a hung tool call; refusing
converts that into one clear line the harness can act on.

## Interactive editing

When stdin is a terminal, Mush uses `rustyline`:

```text
↑ / ↓        previous / next command
Ctrl-R       reverse history search
Ctrl-S       forward history search
Ctrl-A / E   line start / end
Ctrl-W       delete previous word
Alt-B / F    move by word
```

History is stored at `<vfs-root>/.fungos/mush-history` by default:

```sh
mush --root ~/fungos-node --history ~/.local/state/mush/history
```

`MUSH_HISTORY` overrides the path. Non-interactive input still uses a plain
line loop.

## Agent overlay

`mush run op` materializes a VFS workspace into a host-side overlay, starts an
Operator session there, then applies the resulting diff back to VFS.

```sh
mush run op --root ~/fungos-node
mush run op --root ~/fungos-node --prompt "fix the failing tests"
mush run op --root ~/fungos-node -- --model fpl/llm
```

Options:

```text
--config PATH          Soil inference/operator config
--inference-profile    named profile from [inference.profiles]
--operator-bin PATH    default operator-tui, or MUSH_OPERATOR
--base-url URL         override the resolved inference endpoint
--model NAME           override the resolved model alias
--mcp-url URL          override the unibus-mcp bridge endpoint
--no-gateway           send Operator directly to the resolved endpoint
--prompt TEXT          use a one-shot operator run
--no-apply             show the diff and discard the overlay
--keep                 keep the materialized overlay directory
--root PATH            VFS root
```

Inference and MCP configuration comes from [Soil config](soil-config.md).
SSE responses stream through the host inference gateway; request JSON is
scrubbed and upstream credentials are resolved through the configured Geode
adapter. `unibus-mcp`/DCP tools also pass through a loopback MCP gateway:
only allowlisted tools can restore scrubbed values, and results are scrubbed
before returning to the model.
`unibus-mcp`/DCP tools arrive through Operator's MCP bridge; no host PATH is
exposed.

The overlay respects `.gitignore`, `.ignore`, and `.soilignore` in both
directions. Build outputs and binaries covered by those files are never applied
to VFS/CID. Default ignored paths include `.operator/`, `.cache/`, `target/`,
`build/`, `dist/`, `node_modules/`, and `.worktrees/`.

Git metadata under `.git/` is tracked when it is present in the VFS. The overlay
is currently a materialized host directory, not a live FUSE mount. Operator is
still a native process; this slice gives it VFS-derived workspace content and
overlay apply/discard semantics, not yet a host filesystem sandbox.

## Shell syntax

```sh
cmd1 | cmd2        # pipe stdout to stdin
cmd1 ; cmd2        # sequence
cmd1 && cmd2       # run cmd2 only if cmd1 succeeds
cmd1 || cmd2       # run cmd2 only if cmd1 fails
cmd < PATH         # stdin from a VFS file
cmd > PATH         # stdout to a VFS file
cmd >> PATH        # append stdout to a VFS file
cmd 2> PATH        # stderr to a VFS file
cmd 2>> PATH       # append stderr to a VFS file
cmd 2>&1           # merge stderr into stdout
*.txt, docs/*.md   # glob VFS paths; quoted globs stay literal
$(cmd)             # command substitution
# comment          # rest of the line
$?                 # previous status
$0, $1, $#, $@     # script name, arguments, count, all arguments
```

Control flow:

```sh
if test -f docs/a.txt; then echo file; else echo missing; fi
for x in a b; do echo item-$x; done
while false; do echo never; done
for x in a b c; do
  if test "$x" = b; then continue; fi
  echo "$x"
done
```

`break` and `continue` apply to the nearest enclosing loop.

`test` / `[` supports `-e`, `-f`, `-d`, `-s`, `-r`, `-w`, `-x`,
`-z`, `-n`, string `=`/`!=`, numeric `-eq`/`-ne`/`-lt`/`-le`/`-gt`/`-ge`,
and `!`, `-a`, `-o`. `read` consumes one line from piped stdin.

`tgrep` uses the `soil:index` regex candidate index, then reads matching VFS
files for line-level output. This is an indexed `tgrep`-style command, not yet
a literal trigram postings implementation. It supports `-i`, `-s`, `-F`, `-w`, `-v`, `-l`,
`-c`, `-o`, `-n`/`-N`, `-H`/`-I`, `-m N`, `-g GLOB`, `-e PATTERN`,
`--no-index`, and `-q`.

`cat`, `grep`, `head`, `tail` and `wc` read piped stdin when no path is given.
`printf` supports `%s`, `%d`, `%i`, `%b`, `%%`, and the usual backslash escapes.

## Commands

```text
help
pwd
cd [DIR|-]
ls [-l] [DIR]
mkdir DIR...
rmdir DIR...
cat [PATH...]
write PATH [TEXT...]
append PATH [TEXT...]
touch PATH...
head [-n N] [PATH]
tail [-n N] [PATH]
grep [-i] PATTERN [PATH...]
tgrep [OPTIONS] PATTERN [PATH...]
wc [-l|-c] [PATH...]
stat PATH
tree [DIR]
rm [-r] PATH...
mv FROM TO
cp [-r] FROM TO
search QUERY
commands
which COMMAND
source SCRIPT [ARG...]
echo TEXT
printf FORMAT [ARG...]
test EXPR
read [-r] [VAR]
break
continue
export KEY=VALUE
unset KEY...
env
set [KEY=VALUE]
exit
```

## Editing and inspection

`write` replaces a file; `append` reads the current version and writes a new
version with the appended line, preserving VFS history. `CMD > PATH` and
`CMD >> PATH` redirect command output through the same versioned VFS write
path. `mkdir -p`, `cp -r` and `find` are implemented over `soil:vfs` /
`soil:vfs-mut`. `touch`, `head`, `tail`,
`grep`, `wc`, `stat`, `tree` and recursive `rm -r` are implemented entirely over
`soil:vfs` / `soil:vfs-mut`. There is still no host `$EDITOR` or process spawn.

## Git

Mush does **not** run `git` inside the Spore. The Spore has no process, PATH or
network capability. The only git-aware path today is the operator overlay:

```sh
mush run op --root ~/fungos-node
```

which materializes the workspace, lets an external agent run host git, and
applies the resulting diff back to VFS. A native `git` command would be a
separate host action adapter, not a new Mush filesystem capability.

## Scripts

Scripts are Mush commands stored in the VFS:

```text
# scripts/archive.mush
mkdir archive
mv notes/today.md archive/today.md
echo archived
```

```sh
mush --root ~/fungos-node --wasm spores/mush/module.wasm \
  --script scripts/archive.mush
```

Mush scripts are deliberately **not** POSIX shell scripts. There is no host
`/bin/sh`, no `PATH` execution, and no `exec`. A script can only use Mush
commands.

## Storage semantics

```text
write  -> new ContentId/version under the path
mkdir  -> explicit empty directory entry
rmdir  -> remove an empty explicit directory
mv     -> namespace rename; ContentId and history preserved
cp     -> new version with the same bytes
rm     -> namespace unlink; blob and history retained
```

`rm` is not blob destruction. Explicit VFS GC is the collector.

## Worktrees and trees

The first worktree slice lives in the Mush CLI, not a separate `soil`
subcommand. It keeps the VFS as the file store and records the collaborative
objects above it in `soil:records`:

```sh
mush tree snapshot projects/demo          # tree id + file count
mush tree show <tree-id>                  # manifest as JSON
mush tree diff <tree-a> <tree-b>          # + / - / ~
mush tree checkout <tree-id> checkouts/demo

mush worktree create projects/demo --thread thread-1
mush worktree list [--thread thread-1]
mush worktree show wt_...
mush worktree snapshot wt_... [path]
mush worktree checkout wt_... checkouts/demo
mush worktree diff wt_a wt_b
mush worktree schema
```

A **tree** is a content-addressed manifest of a VFS subtree. Each entry carries
the relative path, `ContentId`, size, and media type; the tree id is the BLAKE3
of the canonical entry list. A **worktree** names a VFS path, a thread, a base
tree, and a head tree. A **ref** maps `ref/<worktree>/head` to the current tree.
A **checkout** records where a tree was materialized on a device.

Records use these schemas in the `worktree` collection:

```text
thread     fungos.thread/v1
worktree   fungos.worktree/v1
tree       fungos.tree/v1
ref        fungos.ref/v1
checkout   fungos.checkout/v1
turn       fungos.turn/v1       (reserved)
comment    fungos.comment/v1    (reserved)
```

The records store defaults to `<vfs-root>/.fungos/records`, book `mush`,
collection `worktree`. Override it with `--state-root`, `--worktree-book`, and
`--worktree-collection` (or `MUSH_STATE_ROOT`, `MUSH_WORKTREE_BOOK`,
`MUSH_WORKTREE_COLLECTION`). This is a local records book for the first slice;
point it at a synced book when worktree sync lands.

Limits of this slice:

- Snapshot and checkout are local; there is no mount process or peer sync yet.
- `checkout` writes the tree's files and does not delete extra files that were
  already under the target path.
- Empty directories are not represented; trees are file manifests.
- Tree manifests are flat records, not yet a chunked Merkle DAG. The tree id is
  still content-addressed and stable.
- The mutating subcommands are host-side Mush operations; they do not run inside
  the Spore and do not require the shell component.

## Host boundary

`mush` opens the VFS root, links the grants, and calls the Spore one line at a
time. The Spore returns output, prompt, and exit state. The current host is
line-oriented; `soil:terminal` cell-grid rendering remains the next frontend.

The host also owns command-package lookup. A command runs in a nested Wasmtime
WASI instance with explicit args, piped stdin, the VFS cwd, no inherited host
environment, and only the capabilities configured by the host.

That boundary is what makes `mush` a safe execution layer and what stops it
from replacing an agent's shell: there is no host process, no `PATH`, and no
network. Measure the gap rather than assume it:

```sh
scripts/mush-agent-parity.sh
```

The probe runs the same command lines through the host shell and through Mush
over equivalent content, and reports completions per class. It is only
meaningful alongside honest exit statuses -- a shell that reports success for a
command it could not run appears to pass every toolchain step.
