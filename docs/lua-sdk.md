# Lua SDK from WIT

The Lua SDK is generated from the canonical `wit/` contracts. It is not a
second ABI and it is not UI-only: every free function, resource method, record,
enum, and flag in the WIT graph has a Lua binding. The generated module has no
transport or policy of its own; it calls one host dispatcher.

```sh
cargo run -p soil-sdk-lua -- --wit wit --out crates/soil-sdk-lua/generated
cargo run -p soil-sdk-lua -- --check
```

The checked-in output is:

- `crates/soil-sdk-lua/generated/soil.lua` - the Lua module;
- `crates/soil-sdk-lua/generated/soil-sdk.json` - signatures, resource metadata,
  docs, and type shapes for the host side.

The generator is deliberately thin. It turns WIT names into Lua names and
dispatches with the original WIT interface and function identifiers:

```lua
__soil_call(interface_id, function_name, args, self)
```

`interface_id` is fully qualified, for example `soil:audio@0.1.0/audio`.
`args` is a table of named WIT parameters using the original kebab-case names.
`self` is the opaque host handle for a resource method; it is `nil` for a free
function. A host that does not install `__soil_call` fails loudly at the first
call, which is the correct failure for a missing capability.

## Audio is in the SDK

The generated module includes both layers:

```lua
local soil = require("soil")

-- Portable PCM sink.
local raw = soil.wasi.audio.playback.open({
  format = { ["sample-rate"] = 48000, channels = 2 },
})

-- Room-aware sink. The host resolves `current` or a named room.
local output = soil.soil.audio.audio.open({
  format = { ["sample-rate"] = 48000, channels = 2 },
  room = "current",
})
```

`soil:audio` is the layer a desktop Spore should use when the room matters;
`wasi:audio` is the portable sink underneath it. The Lua SDK does not choose
between them and does not fall back silently.

Resource methods use `:` and the host handle:

```lua
local output = soil.soil.audio.audio.open({ ... })
output:write({ frames = pcm })
output:drain({ ["timeout-ms"] = 5000 })
output:close({})
```

## Result convention

- `option<T>`: the value or `nil`;
- `list<T>`: a Lua array;
- `result<T, E>`: `{ ok = value }` or `{ err = error }`;
- variants: `{ tag = "case", value = value }`;
- flags: a list of flag strings.

This is a binding convention, not a second WIT ABI. The host still enforces
grants, byte limits, deadlines, and room policy.

## Host side

A host installs the dispatcher once and routes it to the same implementations
used by the Rust/Wasm guest bindings. For Soil, that means the host adapters in
`crates/soil-appdata-host`, `crates/soil-vfs-host`, and
`crates/soil-audio-host`. The dispatcher belongs at the Lua runtime boundary,
not inside a Spore: a Spore may call only the interfaces its manifest granted.

## Relationship to SFX

`spores/sfx` keeps its pad **definitions** in `ui/synth.lua`, loaded at runtime
from `sfx/synth.lua` in the VFS. The Rust guest owns the generic synthesizer
and the mapping; the Lua document owns the sound design. The command-line
`sfx` target is the same Spore with a WASI CLI shell, so Mush and Canvas share
the same document and the same room contract.

The generated Lua SDK is the path for a Lua-native host to call `soil:audio`
directly. It is not required for the Rust guest, and it does not replace the
guest WIT bindings; both are projections of the same canonical WIT.
