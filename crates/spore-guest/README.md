# spore-guest

The guest half of a Spore: a Rust library that owns one PUC Lua 5.4 state, loads
the Lua view the package carries, and answers the frozen surface contract
(`soil:surface@1.1.0`) with a `soil-scene` command stream. No renderer, no
window, no GPU, no pixels.

```rust
let mut guest = Guest::new(640, 360)?;
let (state, commands) = guest.present(&Frame { width: 640, height: 360, ..Default::default() })?;
// commands: rect / text / hit / group / clip, in paint order
```

The point is that this is the *same* view code the native host and the browser
run. `Guest::new` bundles `theme.lua`, `ui.lua`, `components/*.lua` and
`view.lua` from `crates/soil-ui-spike` with `include_str!`, so a change to the
view changes every host at once and the parity test fails before a host drifts.

## What it is not (yet)

- **Native only.** `mlua` with `vendored` compiles PUC Lua from C, which builds
  for the host target. A `wasm32-wasip1` build needs the Lua archive from the
  `lua-wasi` fork via `LUA_LIB` / `LUA_LIB_NAME`; the C sources are not
  recompiled by `cc` for WASI here. That is the next step, and the reason the
  browser spike ships `lua.wasm` separately.
- **Not in the workspace.** `mlua` with Lua 5.4 sets `links = "lua"` and the
  Soil workspace already links Luau through `soil-lua`; one Lua per graph. The
  crate carries its own `[workspace]` and lockfile, so run `cargo test` inside
  `crates/spore-guest`; `cargo test -p spore-guest` from the root does not
  select it.
- **No hit-testing.** `input` bumps the generation and returns state. The host
  hit-tests `hit` regions today; routing moves here as the surface migration
  lands.
- **`measure` estimates.** A guest has no shaper, so `measure` returns
  `chars * size * 0.52` and `size * 1.25`, the same estimate the browser page
  uses. Anything that depends on exact wrapping has to be laid out by the host.

## Tests

    cd crates/spore-guest && cargo test

Two tests: the view paints the parity scene (34 commands — 8 rect, 9 text,
12 group, 5 hit, the same split as `crates/soil-ui-spike/parity/native.trace`),
and changing `Frame::selected` changes the painted text, because the view holds
no state of its own.
