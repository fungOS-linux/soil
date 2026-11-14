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
