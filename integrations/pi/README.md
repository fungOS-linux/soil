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
the working directory, and `MUSH_ROOT` a VFS root to use instead of a Desktop —
that is what lets a headless run (`pi -p`) attach without a tty to type `/mush`
at.

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

## MCP (for muse)

The same daemon is also an MCP server, which is how a non-Pi agent uses the
same workspace:

```sh
mush mcp --root DIR --listen 127.0.0.1:PORT --mcp-token "$MUSH_MCP_TOKEN" --foreground
```

The endpoint is loopback by default and always requires the bearer token; the
token is generated when none is given (`--mcp-token` or `MUSH_MCP_TOKEN`).
One server serves one root, and that root is the workspace every call sees.

| tool | contract |
| --- | --- |
| `mush_exec` | one command line. Returns `state=ok` with output and exit status, `state=timeout` when the daemon deadline fired, or `state=background` with a job id when the command outlived its background threshold. |
| `mush_job` | `{job}` checks one job; omit `job` to list. Poll this after `state=background` — do not re-run the command. |
| `mush_cancel` | `{job}` stops one: queued work is dropped, running work is interrupted (guest or host program). |

A `state=background` result means the command is **still running** in the
daemon; its output is not in the tool result, and closing the connection does
not stop it. The job ends when the command does, when `mush_cancel` is called,
or when the daemon exits — job state is in memory, so a daemon restart drops
the record (and kills the child).

VFS commands run in the guest. Allowlisted host programs (`git`, `cargo`,
`gh`, `fpl bob`, ...) run against a materialized overlay of the VFS and apply
their source changes back; build output is ignored in both directions, and
builds go through bob's fleet policy.

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
model chooses tools. Those are the reasons for the agent test.

## Agent test

```sh
scripts/mush-agent-test.sh          # add --keep to inspect the workspace
```

Runs one real headless Pi session — `pi -p`, no builtin tools, no extensions
except this one, only the `mush`/`vfs_*` tools — against a throwaway VFS root
pinned with `MUSH_ROOT`. The task edits a JSON document, appends to a file and
creates a new one, and the script checks the resulting VFS state rather than the
model's prose, so a run that "succeeded" outside the sandbox fails.

Knobs: `AGENT_MODEL` (passed as `--model`), `PI_BIN`, `MUSH_BIN`,
`MUSH_TIMEOUT`. It costs a model call per run; that is the point — it is the
only check that exercises the runtime, the tool swap and the daemon together.
