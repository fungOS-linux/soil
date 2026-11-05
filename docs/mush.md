# Mush

Mush is the sandboxed shell over one VFS workspace. The command engine is a
Wasm Spore; the native `mush` process is only the host.

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
adapter.
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
rm PATH...
mv FROM TO
cp FROM TO
search QUERY
source SCRIPT
echo TEXT
export KEY=VALUE
unset KEY...
env
set [KEY=VALUE]
exit
```

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

`mush` opens the VFS root, links the three grants, and calls the Spore one line
at a time. The Spore returns output, prompt, and exit state. The current host is
line-oriented; `soil:terminal` cell-grid rendering remains the next frontend.
