# Soil shell

`soil` is a small front door to a VFS workspace:

```sh
soil --root ~/fungos-node shell --mount ~/fungOS
```

It starts `fungos-vfs-fuse`, waits for the mount, then launches `$SHELL` with
its working directory inside the workspace. The shell runs on the host pty, so
it also works under `soil-terminal`:

```sh
soil-terminal --command soil --root ~/fungos-node shell --mount ~/fungOS
```

## Safe destructive actions

The FUSE projection already implements the storage rule:

- `rm` removes the namespace entry
- the content-addressed blob is retained
- the version history is retained
- the removal is queued for sync

`soil rm` makes that explicit:

```sh
soil --root ~/fungos-node rm notes/today.md
```

It prints the unlinked path, content id, version, and the history command. The
interactive shell installs an `rm` alias to this command when the shell is
bash or zsh.

Other commands:

```sh
soil --root ~/fungos-node ls [PREFIX]
soil --root ~/fungos-node stat PATH
soil --root ~/fungos-node cat PATH
soil --root ~/fungos-node history PATH
soil --root ~/fungos-node search QUERY
```

`soil search` uses the fuzzy VFS index. It is a code/document locator, not a
shell glob substitute.

## Overlay semantics

The VFS is not a mutable tree of blobs:

```text
write  -> new ContentId/version under the path
rename -> namespace operation, content unchanged
rm     -> namespace unlink, blob and history retained
```

The native mount projects the namespace; it does not own storage. Deleting the
mount or dropping the FUSE process does not delete blobs or history.
