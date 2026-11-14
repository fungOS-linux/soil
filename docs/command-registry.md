# Command registry

`soil-command` is the first virtual PATH: installed wasm commands, not host
binaries.

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

Installing a Spore updates its declared commands and capability set. The
registry refreshes; Mush's virtual PATH and visible command set update without
a shell restart. Profiles filter by declared command role and granted
capabilities, not just by name.

The registry does not grant anything by itself. A command still needs its
host capabilities linked and its profile's write/network/secret scopes.
