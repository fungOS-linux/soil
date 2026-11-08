# Mush

Mush is the sandboxed shell over one VFS workspace. The command engine is a
Wasm Spore; the native `mush` process is only the host.

A Mush session is intended to run inside a **Desktop**, the durable top-level
namespace for a body of work. `--root` is currently a standalone VFS root; the
Desktop binding adds project mounts, a records scope, and a stable identity. See
[Desktops](desktop.md).

```sh
mush --root ~/fungos-node \
  --wasm spores/mush/module.wasm
```

The Spore imports exactly three capabilities:

```text
soil:vfs       read, stat, list
soil:index     browse, search, KNN
soil:vfs-mut   write, remove, mkdir, rmdir, rename
```

It has no host filesystem, `PATH` lookup, subprocess, or network access.

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

## Commands

```text
help
pwd
cd [DIR|-]
ls [-l] [DIR]
mkdir DIR...
rmdir DIR...
cat PATH
write PATH [TEXT...]
append PATH [TEXT...]
touch PATH...
head [-n N] PATH
tail [-n N] PATH
grep [-i] PATTERN PATH...
wc [-l|-c] PATH...
stat PATH
tree [DIR]
rm [-r] PATH...
mv FROM TO
cp [-r] FROM TO
search QUERY
source SCRIPT
echo TEXT
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

`mush` opens the VFS root, links the three grants, and calls the Spore one line
at a time. The Spore returns output, prompt, and exit state. The current host is
line-oriented; `soil:terminal` cell-grid rendering remains the next frontend.
