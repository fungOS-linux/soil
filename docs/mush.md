# Mush

Mush is the sandboxed shell over one VFS workspace. The command engine is a
Wasm Spore; the native `mush` process is only the host.

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
```

```sh
mush doctor
```

`doctor` reports the effective VFS root, command root, records binding, Spore
source, principal and service socket. Missing roots are errors, not silent
fallbacks.

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
mush mcp --listen 127.0.0.1:0
```

It exposes `mcp__mush__mush_exec` over loopback HTTP. See
[Operator integration](mush-operator.md).

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
```

`test` / `[` supports `-e`, `-f`, `-d`, `-s`, `-r`, `-w`, `-x`,
`-z`, `-n`, string `=`/`!=`, numeric `-eq`/`-ne`/`-lt`/`-le`/`-gt`/`-ge`,
and `!`, `-a`, `-o`. `read` consumes one line from piped stdin.

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

## Host boundary

`mush` opens the VFS root, links the grants, and calls the Spore one line at a
time. The Spore returns output, prompt, and exit state. The current host is
line-oriented; `soil:terminal` cell-grid rendering remains the next frontend.

The host also owns command-package lookup. A command runs in a nested Wasmtime
WASI instance with explicit args, piped stdin, the VFS cwd, no inherited host
environment, and only the capabilities configured by the host.
