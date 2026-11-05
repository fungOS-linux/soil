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
