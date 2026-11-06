# consent

Native host consent popup. The broker is a host crate; this Spore is the
replaceable system surface.

The popup listens on a per-user Unix socket, blocks each caller until a
decision, and renders one request at a time. Canvas/Spore-composed previews come
later.

```sh
cargo run -p soil-consent-popup
```

The socket lives in a private `0700` per-user directory and is mode `0600`.
No UI, closed UI, cancel or timeout denies.

`module.wasm` is a minimal package identity module. The native binary is
`soil-consent-popup`.
