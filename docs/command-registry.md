# Command registry

`soil-command` is the first virtual PATH: installed wasm commands, not host
binaries. A shell reads two roots, in this order:

```text
~/.local/share/fungos/commands/   what an operator installed, one package per name
~/.local/share/fungos/spores/     what a store installed; a Spore declares its commands
```

```text
~/.local/share/fungos/commands/
  mb/
    command.toml
    module.wasm
```

```toml
schema = 1
[command]
name = "mb"
version = "0.1.0"
summary = "Marbles task tracker"
module = "module.wasm"
[limits]
timeout_ms = 120000
max_stdout_bytes = 4194304
max_stderr_bytes = 1048576
max_memory_bytes = 268435456
```

```sh
soil-command --list
soil-command --records-root ~/.local/share/fungos/records \
  --book personal/avery/marbles --collection marbles \
  --env MARBLES_ACTOR=avery \
  run mb --project elmers list
```

The runner uses WASI stdio, memory/output caps and an epoch timeout. It links
`soil:records` only when a records binding is configured. Commands never get
host PATH, host filesystem, network or a Marbles-specific host.

## Roots and precedence

The roots are siblings under the fungOS data directory, so one setting moves
both: `--command-root` / `FUNGOS_COMMAND_ROOT` (and `MUSH_SPORE_ROOT` to split
them apart). The command root comes first, so a hand-installed command shadows a
Spore that later declares the same name.

Nothing is copied at install time. `soil-install` writes a tree to
`spores/<name>/`, and the registry reads that tree's manifest where it sits,
which is why an install shows up in a running shell at its next prompt instead
of needing a link, a copy, or a restart.

A package that cannot be read - a bad manifest, a module it names but does not
carry - is refused *by name* and the rest of the PATH still answers. A name
claimed twice is refused the same way, with the losing directory recorded.
Neither is fatal: a shell loses one command, not every command, and `mush doctor`
and `soil-command --list` both print what was refused.

`soil-install` refuses a package that names a component it does not carry before
it replaces anything, and prints the commands an install brought, so the
promise is checked at the boundary rather than at the first run.

Commands do not inherit the host environment. `--env NAME=VALUE` is the only way
a variable reaches one, so what a command can see is what a caller wrote down.
The output caps are per command (`command.toml` `[limits]`): a command that
answers with megabytes needs `max_stdout_bytes` sized for it, and a write past
the cap surfaces as an error to the command rather than as a truncated answer.

## Shells that are not Mush

`soil-command shims --dir DIR` writes one executable per command, so zsh - or any
other shell - resolves a Spore's program like any other binary:

```sh
soil-command shims --dir ~/.local/share/fungos/bin   # then put that on PATH
```

The runner and the roots are baked into each shim, so it works from any directory
and does not depend on `FUNGOS_COMMAND_ROOT` being exported. A shim carries no
policy of its own: it sources `~/.config/fungos/commands/<name>.env`, a plain
`KEY=VALUE` file holding the records binding (`FUNGOS_RECORDS_ROOT`,
`FUNGOS_RECORDS_BOOK`, `FUNGOS_RECORDS_COLLECTION`) and anything else a person
decides a command may have.

A file the generator did not write is never touched. `mb` keeps a hand-written
shim because a book and an actor are decisions, not defaults, and replacing it
would be a data-loss bug rather than an update. `--prune` removes only shims it
wrote, and only for commands that are gone.

A command whose package declares a `soil:*` capability this runner does not link
is refused by name at scan time, with the reason. A PATH entry that traps on
start is worse than an honest refusal.

## Warm

`soil-command warm` compiles every registered command into wasmtime's on-disk
cache and writes the module's digest beside the package, in `.warm/<name>.json`.
A component is in that cache only once it has been compiled, so compiling *is*
the warm step; the marker is what lets a later reader tell warm from merely
installed without compiling again - `warmed mb` the first time, `already warm
mb` after that.

Only a host can warm what it will run, so this covers the cache `soil-command`,
`mush` and every engine built the same way share. The launcher keeps its own
`.cwasm` files and warms them itself.

## Profiles and installed Spores

The next layer is profile-aware resolution:

```text
human profile: interactive commands + full workspace
agent profile: allowlisted commands, write scopes, quotas, no editors
```

Installing a Spore is enough: the registry re-scans the install root on every
lookup, so Mush's virtual PATH and visible command set update without a shell
restart. Profiles filter by declared command role and granted capabilities, not
just by name.

The registry does not grant anything by itself. A command still needs its
host capabilities linked and its profile's write/network/secret scopes.
