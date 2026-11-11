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

### One-shot commands use it automatically

Compiling the Spore costs about 13 seconds of CPU. Paying that for a single
`pwd` is what makes `mush sh -c` unusable as an agent's shell, so a one-shot
command (`sh -c`, `--command`) forwards to a daemon for its VFS root instead:

```sh
mush --desktop personal sh -c 'pwd'   # 11s: starts the daemon, compiles once
mush --desktop personal sh -c 'pwd'   # 25ms
```

- The socket is per VFS root, under `~/.local/share/fungos/mush/run/`, so two
  Desktops never share a command engine. `--socket` overrides the path.
- The daemon detaches into its own process group, so the terminal that started
  it can hang up without taking it down.
- It stops after 30 minutes idle (`--idle-timeout`) and removes its socket.
- The caller's `--timeout` becomes the daemon's background threshold: a command
  you would wait 15 minutes for is not handed back as a job id after 60s.
- `--no-daemon` (or `MUSH_NO_DAEMON=1`) runs in-process. Tests use it, because
  a daemon started by a test outlives the test and holds the records store.
- Requests are served one connection per thread, so `mush jobs` and
  `mush cancel JOB` reach a daemon that is busy. `cancel` drops queued work and
  traps a running guest through the same epoch the deadline uses.
  A host program (`git`, `cargo`, `awk`) is an OS process rather than guest
  code, so it is spawned, polled, and killed on cancel — and `--timeout` now
  interrupts it the same way it interrupts the guest (exit 124 for a deadline,
  130 for a cancel, with the reason on stderr).

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
Tab          complete command names and VFS paths
Ctrl-A / E   line start / end
Ctrl-W       delete previous word
Alt-B / F    move by word
```

Tab completes the first word against builtins plus installed command packages,
and completes VFS paths everywhere else, relative to the shell's current
directory. Completion runs in the host because only the host can see the
command packages; the shell's `cd` stays in the Spore, so the host refreshes its
idea of the directory at start-up and after every `cd`.

### Themed prompts

`PS1` is enough for a static prompt. For anything richer, point Mush at a prompt
renderer and it uses its stdout as the prompt:

```toml
# ~/.config/fungos/mush/config.toml
prompt_command = "starship prompt --status $MUSH_STATUS --cmd-duration $MUSH_DURATION_MS"
```

`MUSH_STATUS`, `MUSH_DURATION_MS`, `MUSH_CWD`, and `MUSH_HOST_CWD` are exported
for the command, along with `STARSHIP_SHELL=mush`. [Starship](https://starship.rs)
needs no adapter beyond that line; any script that prints a prompt works the
same way. If the command fails or prints nothing, Mush falls back to the Spore's
own prompt rather than leaving the session without one.

When the Desktop is projected on this device, the command also runs **inside
that projection**, so a renderer that inspects a directory reads real files
rather than the VFS path it cannot open. That is what lets a Git segment report a
branch when the projected project is a Git checkout, and leaves it empty when it
is not. `MUSH_CWD` stays the VFS path; `MUSH_HOST_CWD` is where it maps to here.

Note that Spaceship is a Zsh theme -- it is shell code, so it cannot be a Mush
prompt renderer. Starship's presets cover the same look without depending on a
shell Mush does not have.

The renderer runs in the host, not the Spore, because the Spore has no process
and no network. A themed prompt therefore describes the VFS (directory, status,
duration, time) rather than a Git branch -- the VFS has no repository state to
read.

History is stored at `~/.local/state/fungos/mush/history`, a stable per-user
path. History that lived inside the VFS root would be empty every time the root
changed, which leaves Ctrl-R with nothing to search.

```sh
mush --root ~/fungos-node --history ~/.local/state/mush/history
```

`MUSH_HISTORY` overrides the path. Non-interactive input still uses a plain
line loop.

### No job control

Ctrl-Z, `fg`, `bg` and `&` do not exist here, and cannot: they are operations on
child processes, and Mush runs every command in-process inside the Spore. Ctrl-Z
prints a one-line explanation instead of suspending, because stopping the shell
mid-read would leave the terminal in raw mode with nothing to resume.

Backgrounding exists at a different layer. A daemon call that outlives its
threshold keeps running and hands the caller a job id:

```sh
mush request --background-after 600 'mb ready --json'
# command still running after 600s; check it with `mush job j1`
mush jobs
mush job j1
```

That is the receipt an agent needs; it is not shell job control.

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

## Diff and JSON

`diff LEFT RIGHT` is a line-level unified diff computed inside the Spore: no
overlay, no host process, and both buffers are read through `soil:vfs`. It
exits 0 when the files match, 1 when they differ, and 2 when a side cannot be
read — so errors go to stderr and never look like a difference. `-U N` sets
the context (default 3), `-q` prints only whether they differ, a NUL near the
start of either side reports a binary difference, and a missing final newline
prints `\ No newline at end of file` because it is content. Diffs larger than
1,000,000 line pairs (the LCS table is bounded) degrade to "every line
replaced" instead of allocating without limit.

`json` reads and edits documents with dot/bracket paths: `a.b[0]`, a bare
numeric segment as an index, and `["a.b"]` for a key that contains a dot.
`get` prints strings raw (`-j` keeps the JSON encoding) and everything else as
compact JSON. `set`, `del` and `merge` read the file version, mutate, and write
back through the same versioned `vfs::write` path as `write` and `sub`, so a
concurrent edit fails the write instead of being clobbered.

Editing rules, so a rewrite stays reviewable:

- `set` replaces a value and creates only the final segment; a missing parent
  path is an error. An array index equal to the length appends.
- `merge` is deep for objects and replaces everything else, arrays included.
- Key order is preserved, and the file keeps its own indentation (2, 4, tabs,
  or compact) and whether it ended with a newline. Everything else is
  normalized by `serde_json`.
- `json pretty` never writes; `json set FILE PATH -` reads the value from
  stdin, so `json get a.json x | json set b.json y -` works without escapes.

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
find [ROOT] [PATTERN] [-name GLOB] [-type f|d] [-maxdepth N]
sub PATTERN REPLACEMENT PATH...
diff [-q] [-U N] LEFT RIGHT
json get [-j] FILE [PATH]
json set FILE PATH JSON|-
json del FILE PATH
json merge TARGET SOURCE...
json pretty [--indent N] [FILE|-]
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
man NAME
exit
```

## Spore identity reads

`man NAME` prints the generated manual of an installed Spore, and `NAME
--version` prints its identity line. Both are host builtins: the Spore owns
shell semantics, but package metadata lives behind the host, so the host answers
and the guest's authority is unchanged.

Spore packages are read from `~/.local/share/fungos/spores` (a sibling of the
command root), or `MUSH_SPORE_ROOT` when set. `man` works with no display and no
network: the page is generated from the installed manifest and the body ships
inside the package.

## Editing and inspection

`write` replaces a file; `append` reads the current version and writes a new
version with the appended line, preserving VFS history. `CMD > PATH` and
`CMD >> PATH` redirect command output through the same versioned VFS write
path. `mkdir -p`, `cp -r` and `find` are implemented over `soil:vfs` /
`soil:vfs-mut`. `touch`, `head`, `tail`,
`grep`, `wc`, `stat`, `tree` and recursive `rm -r` are implemented entirely over
`soil:vfs` / `soil:vfs-mut`. There is still no host `$EDITOR` or process spawn.

`sed -i 's/pat/rep/g' FILE...` is answered by `sub` when the script is one where
sed and the Spore's regex agree: `g` present, no backslashes, none of
`+ ? | ( ) { }` in either half, and no `&` or `$` in the replacement. Everything
else — `-i.bak`, a second script, a form without `g` — runs the real `sed`
against the overlay, so a form the bridge does not recognise is never silently
approximated. `sub` compiles its pattern in multi-line mode, so `^` and `$` mean
the start and end of a line, as they do in sed.

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

## Host commands

`mush run host` materializes the VFS into a temporary overlay, runs one
allowlisted host command there, and applies the resulting source changes back
to the VFS. Build outputs (`target/`, `node_modules/`, `dist/`, …) are ignored
in both directions, so a build never writes artifacts into the VFS.

```sh
mush run host -- cargo build --release
mush run host --cwd crates/app -- cargo test
mush run host --no-apply -- cargo fmt --check
```

The environment starts empty and is rebuilt from a small allowlist (`PATH`,
`HOME`, `TMPDIR`, the Rust and sccache variables, `BOB_AGENT_KEY`). Add more
with `--allow-env NAME`. The command must be a bare name on the allowlist
(`cargo`, `rustc`, `cc`, `clang`, `clang++`, `c++`, `git`, `make`, `cmake`,
`ninja`, `pkg-config`); shells and paths such as `./cargo` are rejected.

Exit status is preserved. `--no-apply` discards the overlay instead of writing
changes back, and `--keep` prints the overlay path for inspection.

This is a policy boundary, not a sandbox: an allowed program runs as the user
and can read their home. It exists so a host toolchain can operate on the VFS
view without handing the agent's secrets to it.

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

## The host bridge

The Spore has no host process. That is the boundary that makes it safe, and it
is also what stops it from being an agent's shell: `git` and `cargo` live on the
other side of it.

`mush sh` crosses that boundary for a short list of programs. The VFS is
materialized into an overlay, the program runs there, and its changes are
applied back:

```sh
mush sh -c 'git status && cargo test'
mush sh -c 'rg TODO src'
```

The allowlist is compilers, version control, and text tools:

```text
cargo rustc cc clang clang++ c++ git make cmake ninja pkg-config
rg sed awk diff jq true false test
```

These run with the user's full privileges and only the overlay as their working
directory -- they can still read `$HOME`. Anything that carries credentials or
reaches the network (`gh`, `ssh`, `curl`, `python3`) is therefore **not** in the
list: adding one is a boundary decision, not a convenience. A bare program name
only is accepted, so `./cargo` cannot smuggle in code from the overlay itself.

Pipelines and redirection are handed to the Spore, which owns that parsing; a
simple command whose program is allowlisted goes to the host. Screenshots of the
gap: `scripts/mush-agent-parity.sh`.

## What is in a Desktop

An agent should ask the Desktop what it contains rather than being handed a
hand-written list in every session.

```sh
mush projects                 # this directory's Desktop
mush projects --desktop acme
mush projects --json
```

```text
project                  source    branch         state   remote
missing-repo             missing   -              -       
plain-dir                found     -              -       
repo-a                   declared  master         dirty   [marbles]
```

`source` is the difference between the declared list and what is on disk:

| source | meaning |
|---|---|
| `declared` | named in the Desktop config and present |
| `missing` | named in the config, nothing on the projection |
| `found` | on the projection, not named in the config |

Branch, dirtiness, and remote come from the host `git` in the projection, so
this describes what you can actually work on. A project without `.git` reports
`-` rather than a guess, which is how the current NFS projection behaves: its
projects are directories, not checkouts.

`[marbles]` marks a project carrying a `.marbles` or `.marbles-local` directory.

Not in this slice yet: joining `mush worktree` records per project, and
resolving the marbles project id through `mb` rather than a marker directory.

### Remote discovery

```sh
mush projects --remote            # GitHub plus the Gitea mirror, deduped
mush projects --claim NAME...     # group repositories into this Desktop
```

`--remote` asks `gh repo list` for every repository the authenticated account
can see, and the Gitea mirror when the machine is pointed at one:

```sh
export GITEA_URL=https://gitea.example
export GITEA_TOKEN=...            # repo read scope
export GITEA_USER=...             # optional; defaults to the token's user
```

Hosts are merged on the lowercased repository name, so a mirror is one project
with two hosts, not two projects:

```text
project                  source    branch   state   hosts        remote
fungos-vfs               declared  -        -       github+gitea
soil                     -         -        -       github
ajmwagar.github.io       -         -        -       github       [archived]
```

Without `GITEA_URL`/`GITEA_TOKEN` the mirror is simply absent -- that is a
configuration state, not an error.

`--claim` writes names into the Desktop's `projects` list. It declares, it does
not clone: a claimed repository that is not on the projection reports `missing`
until something materializes it, which keeps the declaration honest.

### Materializing a project

```sh
mush project add NAME --desktop acme                 # claim + shallow clone
mush project add NAME --desktop acme --worktree w1   # plus a CoW worktree
```

The clone is `--depth 1` on purpose: a starting point for work, not an archive.
The worktree is a copy-on-write copy of that clone:

| platform | primitive | cost |
|---|---|---|
| macOS / APFS | `cp -c` (clonefile) | near zero |
| Linux / btrfs, xfs | `cp --reflink=auto` | near zero |
| other, and NFS | plain copy | full size |

That last row matters: a Desktop projected over NFS gets a real copy, not a
reflink, so the worktree costs what the tree costs. The command says which
primitive it used rather than implying CoW everywhere.

Verified on APFS: shallow clone, `clonefile` worktree, and the base clone of a
small repository at 144K.

### Resolving a worktree

`mush origin` now reads the Desktop worktree shape as well:

```sh
cd ~/mnt/personal/worktrees/agent-1/addict
mush origin
```

```text
worktree: agent-1
marble project: agent
repo: addict
desktop: personal
mount: /Users/me/mnt/personal [mounted]
project: /Users/me/mnt/personal/projects/addict
```

`worktree` is the label of the tree the agent is inside, `repo` is what it is a
tree of, and `marble project` comes from the `.marbles/project.toml` nearest the
working directory. Together with `thread` (from `DELTA_CURRENT_THREAD_ID`) that
is the identity of a working agent: who, on what, in which tree.

The thread <-> marble link is not resolved here yet; it needs the claim record,
not the filesystem.

### Who a host command is running as

Every host program started by `mush sh` receives the caller's identity in its
environment, so a tool can record *which* agent did something instead of leaving
the link to be reconstructed later:

| variable | meaning |
|---|---|
| `MUSH_THREAD_ID` | the Delta thread that owns this session |
| `MUSH_WORKTREE` | the worktree label (`agent-1`) |
| `MUSH_PROJECT` | the project the tree is of |
| `MUSH_MARBLE_PROJECT` | marbles project slug from `.marbles/project.toml` |
| `MUSH_DESKTOP` | the Desktop that owns the tree |

Unset variables are omitted rather than empty: a consumer cannot tell which
half of a half-known identity is a guess.

This is the contract behind "jump to the agent working this marble" --
`mb claim` can record the thread id it was handed, and `mb show` can then print
a `delta://thread/...` link. The marbles side is not wired yet.

## Shell integration

`mush sh` is the POSIX-ish entry point, and the binary also answers the two ways
a login shell is invoked.

```sh
mush sh -c "cargo check && git status"   # one command line
mush sh                                  # read lines from stdin
mush -c "pwd"                            # no `sh` needed
mush -lc "pwd"                           # a terminal's login+command form
```

The command line splits on `;`, `&&` and `||`. VFS commands (`cd`, `ls`, `cat`,
`grep`, `mkdir`, `rm`, `mv`, `cp`, `export`, …) run inside the Spore; allowlisted
host programs (`cargo`, `rustc`, `cc`, `git`, `make`, …) run through the overlay
bridge. Anything else is refused with exit 127. Shell state (`pwd` and exported
variables) is persisted per session under `<vfs-root>/.fungos/sessions/<key>.json`,
because a terminal spawns a fresh process for every command.

### Login shells

A login shell is invoked as `-mush` (argv[0] with a leading dash) or with `-l`.
Nothing sources `/etc/profile` or `~/.profile` for it, so mush builds its own
environment: `PATH` becomes the user toolchain first (`~/.cargo/bin`,
`/opt/homebrew/bin`, `/usr/local/bin`) followed by the system directories and
whatever `PATH` it inherited, deduplicated and filtered to directories that
exist. `MUSH_LOGIN=1` is set. That PATH is what the host bridge inherits, which
is why `mush -lc "cargo build"` finds a toolchain.

The Spore's own `PATH` stays `/commands:mb`: it resolves VFS commands, not host
programs, and conflating the two would make a host binary shadow a VFS command.

### Scripts and shebangs

```sh
#!/usr/local/bin/mush
cd projects/demo
pwd
```

`mush path/to/script.mush` runs the file line by line through the same engine.
The `#!` line is stripped on the host — the Spore has no comment marker to skip
it with. Scripts are host files (the kernel resolved the path); the Spore's
`source` reads the VFS instead, so use it for VFS content.

### `/bin/mush`

On macOS `/bin` is on the sealed system volume: even root cannot create
`/bin/mush` (`Operation not permitted`). Install the binary at
`/usr/local/bin/mush` and, if `chsh` should accept it, add that path to
`/etc/shells` (a root edit). `sshd` uses the passwd shell directly and does not
consult `/etc/shells`; `chsh` does.

A login shell of `mush` is a shell over the VFS, not over the host filesystem:
it has no `~/.zshrc`, no host `$EDITOR`, and no host process spawn outside the
bridge. That is the point, but it means it is the right login shell for an agent
session, not for a human's everyday terminal.
