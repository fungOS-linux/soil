# soil-input

Host-neutral pointer, key, focus and IME events.

```text
winit ──┐
Canvas ─┼──► soil_input::InputEvent ──► producer / router
Web ────┘
```

The platform adapter owns raw keycodes and window handles. Producers see:

- surface-local normalized pointer coordinates in `0..=1`
- semantic keys (`arrowup`, `enter`, `f1`, one character)
- modifiers, repeat, text, IME preedit/commit
- focus transitions
- millisecond timestamps supplied by the host

`soil-input` also owns `terminal_bytes`, the shared xterm key encoder used by
native terminal windows and future web terminals.

`InputEvent` serializes to the Canvas producer protocol:

```json
{"kind":"pointer","phase":"press","x":0.25,"y":0.75,"shift":false}
{"kind":"key","key":"arrowup","shift":false}
```

Winit adapters are behind the `winit` feature. The core crate is WASM-safe and
does not depend on a windowing system.

## Ports

- Canvas (iced): pointer, scroll and key events become `InputEvent` values
  before crossing `spore_socket`.
- canvas-web: DOM pointer, wheel and key events use the same types before the
  loopback bridge.
- native Soil: `soil-viz` and `soil-vt-window` build the same events from winit,
  then route them to the live host or `terminal_bytes`.
