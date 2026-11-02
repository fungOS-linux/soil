# Permissioned D-pad navigation — reversal-ik3w

Soil sole-writer source integration with separate Canvas/GFX owner. Existing
battery guest exports input(i32) and calls Canvas navigate; no guest replacement
or host-navigation bypass required. Actual Wasm input runs with fuel and existing
Canvas import budgets. Estimate imports are not enabled by production CLI.

CLI keypad socket/allow/deny flags now parse independently of battery/UI flags;
no positional argslen limitation combining these capabilities. Allowed keypad
requires explicit host socket; socket requires allow/deny choice. Denied grants
construct no client and make no connection. Inner is sole input owner; outer
--allow-keypad fails before guest construction/connection. Outer stays passive.

Normalized physical bindings remain ONLY D-pad103/108/105/106, Enter28, Back158.
Home172/Recents580 evidence is not yet bound in Soil; no live physical Home,
Recents, sidevolume or PTT claim. No volume backend/contact/optimistic levels.
Owner default overlay is hidden, permanent volume unavailable line removed.

Views: Home menu, Battery, Device status. Left/Right bounded without wrap;
Up/Down bounded Home focus; Enter opens focus; Back resets Home/focus. Battery
retains existing measured BatteryView; estimate is optional Battery-only and
NOT enabled in deployment. Device services unavailable is honest status.

Frozen-source gates:56 workspace tests PASS, all-target check and strict Clippy
PASS. Includes actual battery-Wasm input at inner480x640 and outer128x128,
combinedCLI typedkeypad+battery+simplejarvis, denied no contact, outergrant
rejection, stale/disconnect/reconnect heldclear, freshpress and directionalrepeat.
Old Select-noop test updated to Selectopens/Backrestores, not waived; estimate
nativepixel test now navigates Battery through Wasm before Fresh!=Unknown.
Logs `/tmp/soil-navigation-final-tests.log`, `/tmp/soil-navigation-build.log`.

Parent keypad-only provider is independently deployed, explicit
`/run/fungos-keypad-provider.sock`, FUNGOS_INPUT_NAME=keypad. Parent owns lifecycle;
no Soil raw evdev, full backend, lid/presenter/DRM changes. Inner poll50..100ms
recommended (eventmaxage1000ms), not1000ms. Outer gets NO keypad grant.

Parent staging example (retain actual existing production output paths):

```sh
/run/soil-canvas-navigation --guest /run/battery-canvas.wasm \
  --output /tmp/navigation-inner.bgr --frames 36000 --interval-ms 50 \
  --ui simple-jarvis --battery-socket /run/fungos-battery-provider.sock \
  --allow-battery --keypad-socket /run/fungos-keypad-provider.sock \
  --allow-keypad --surface inner

/run/soil-canvas-navigation --guest /run/battery-canvas.wasm \
  --output /tmp/navigation-outer.bgr --frames 36000 --interval-ms 250 \
  --ui simple-jarvis --battery-socket /run/fungos-battery-provider.sock \
  --allow-battery --surface outer
```

Atomic BGR24 inner921600bytes, outer49152bytes. Preflight use frames1 first;
input motion needs subsequent frames after initial provider cursor head sync.
Outer displays its own passive Home view, not mirrored navigation state.

Single isolated incremental build target `target/soil-canvas-navigation`, jobs2.
Clean rollback binary target/soil-canvas-clean remains a40f913693739721afb72305bc1eaaea5f112e5fc579542d207808f974ddbb88.
Battery guest retained f2da070c80d60f0426d77c3f8d368ba325c5d9a6a4bfed6f25d705640862214f.
Verified build artifact `target/soil-canvas-navigation/aarch64-unknown-linux-musl/release/soil-canvas`: static stripped ARM64 ELF, SHA256
`b3f9b0488bae67fbb4e9df43a5f3cc1bd3bd0835b0e3251f3c8242092e172c09`.
Build completed28.21s. Only unsupported dependency cdylib/deprecated linkeropt warnings.
Generated battery guest matches existing f2da07 exactly; no guest replacement needed.
