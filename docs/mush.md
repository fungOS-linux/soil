# Mush

Mush is the sandboxed Shell over one VFS workspace.

```sh
mush --root ~/fungos-node
```

It has no host filesystem, `PATH`, environment, subprocess, or network access.
Every path is workspace-relative. Every mutation is a VFS namespace operation.

## Commands

```text
help
pwd
cd [DIR]
ls [DIR]
cat PATH
write PATH [TEXT...]
rm PATH...
mv FROM TO
cp FROM TO
stat PATH
history PATH
search QUERY
source SCRIPT
echo TEXT
exit
```

## Scripts

Scripts are Mush commands stored in the VFS:

```text
# scripts/archive.mush
mv notes/today.md notes/archive/today.md
echo archived
```

```sh
mush --root ~/fungos-node --script scripts/archive.mush
```

Mush scripts are deliberately **not** POSIX shell scripts. There is no host
`/bin/sh`, no `PATH`, and no `exec`. A script can only use Mush commands.

## Storage semantics

```text
write  -> new ContentId/version under the path
mv     -> namespace rename; ContentId and history preserved
cp     -> new version with the same bytes
rm     -> namespace unlink; blob and history retained
```

`rm` is not blob destruction. Explicit VFS GC is the collector.

## Status

This is the first sandboxed authority slice: a native process whose only data
access is the `fungos-vfs` root given on the command line. It is not yet a Wasm
Spore and does not yet use `soil:terminal`.

The next slice is the same command engine as a component with:

```text
soil:vfs      read
soil:vfs      mutation (separate grant)
soil:index    search
soil:terminal cell grid + input
```

At that point `mush` runs inside Soil with no native filesystem or process
authority at all.
