# Mush for Pi

`mush.ts` is a [Pi](https://github.com/earendil-works/pi) extension that turns a
session's shell into Mush on a chosen Desktop. While it is on, `/mush`:

- replaces the host shell (`bash`) and the host file tools (`read`, `write`,
  `edit`, `grep`, `find`, `ls`) with `mush` and `vfs_*` tools, and blocks the
  host tools if something else re-activates them;
- runs every command through `mush --desktop <desktop> sh -c`, so VFS builtins
  stay in the Spore and allowlisted host programs (`git`, `cargo`, `gh`, `rg`,
  `sed`, `make`, …) run against an overlay of the same VFS;
- tells the model, in the system prompt, that paths are VFS-absolute and the
  working directory is `/`.

The Spore and the host bridge live in this repository; the extension is only
the client. This directory is the source of the installed file.

## Install

```sh
ln -sf "$PWD/integrations/pi/mush.ts" ~/.pi/agent/extensions/mush.ts
```

Then `/mush <desktop>` in a session, or `/mush off` to put the host shell back.
`MUSH_BIN` overrides the binary, `MUSH_TIMEOUT` the per-command deadline (900s),
`MUSH_DESKTOP` the Desktop used when `mush origin` cannot tell which one owns
the working directory.

`/mush` state is per session: a reload re-attaches by looking at the active tool
set, which is why `tryAutoEnable` runs on use rather than at load.

## Tools

| tool | what it is |
| --- | --- |
| `mush` | the shell: one command line, chained with `;`, `&&`, `||` |
| `vfs_read` / `vfs_write` / `vfs_edit` | files, byte for byte, through `printf '%s'` |
| `vfs_grep` | `tgrep`, the indexed regex search |
| `vfs_find` | the Spore's `find -name`, glob over the VFS |
| `vfs_ls` | `ls -l`, rendered |
| `vfs_diff` | the Spore's unified diff; differences are a result, not an error |
| `vfs_json` | `json get/set/del/merge/pretty` with dot/bracket paths |

## Test harness

```sh
bun integrations/pi/harness.ts
```

The harness loads `mush.ts` the way Pi does (with `typebox` resolved from the
Pi install), stubs the Pi API, points `MUSH_BIN` at a wrapper that answers
`origin`/`desktop`/`projects` and forwards `sh -c` to the real binary against a
scratch VFS, then calls every tool and checks the text and the error flag. It
exits 0 when no `mush` binary is on `PATH`, so a checkout without Mush still
tests clean.

What it does not cover: the Pi runtime itself (tool activation, `/mush` state
machine, the system-prompt hook), a real Desktop, and anything about how the
model chooses tools. Those are the reasons to run a real session.
