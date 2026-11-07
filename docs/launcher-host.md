# Launcher host: macOS, activation and app launch

Status: design, 2026-11-07. Companion to `spores/docs/launcher.md`.

That document defines the launcher Spore. This one defines what the **host** must
provide for it to exist on macOS, and draws the line between a capability and a
host policy surface so neither drifts.

## The line

A launcher needs things no Spore may have: a global hotkey, a window over another
app, a list of installed applications, and the authority to start one. None of
that should become WIT the way `soil:canvas` did, because none of it is
transportable — a headless fungOS node has no hotkey.

So the split is:

| Concern | Shape | Why |
|---|---|---|
| Query → locators | `soil:search`, `soil:spore` | already transportable |
| Row → effect | `fungos:actions` | already transportable, exact-version grants |
| Window, hotkey, modality | host policy | device-specific, not grantable |
| Installed app inventory | `soil:search` source | host supplies it like peers/services |
| Starting an app | `fungos:actions` action | effect with an audit trail |

Everything a Spore can *see* is a locator. Everything that *does* something is an
action. Nothing in between.

## 1. Canonicalise `soil:spore`

`soil:spore@0.1.0` exists today only as a vendored copy inside
`spores/launcher/wit/deps/soil-spore/spore.wit`. It is the installed-package
catalog and it is what makes a launcher possible at all.

- Move it to `wit/soil-spore.wit`.
- Add the row to the capability table in `README.md` with grant
  `soil:spore.catalog` (and `soil:spore.launch` if launch is separated from
  listing).
- Add coverage in `crates/soil-abi` so the reviewed name and functions are
  asserted and an accidental change fails the build.
- Keep `launch` a separate grant from `entries`. Listing installed packages and
  running one are different statements of intent, and the current package grants
them as one string.

## 2. The macOS Canvas presenter

This is the real gap. `soil:canvas` has three owners today:

| Surface | Crate | Platform |
|---|---|---|
| Wayland/DRM | `soil-runtime` + `s22-canvas-surface` | Linux |
| Android | `soil-android` (JNI) | S22, Fire TV |
| Browser | `canvas-web` | external |

There is no macOS owner surface. Nothing in this workspace can put a pixel on a
Mac screen.

Two crates already prove the parts:

- **`soil-consent-popup`** is a native window that starts *hidden* and appears
  when a request arrives. That is the launcher's exact lifecycle. It uses
  `eframe =0.33.3` with `wgpu` and `wgpu 27`'s Metal backend.
- **`s22-canvas-surface`** is a real owner surface (frame, generation, key
  mapping into egui events) rendered with egui and `egui_software_backend`.
- **`soil-canvas-input`** already has `Key::from_macos_keyboard` and
  `apply_macos` for special keys, with the correct note that composed text is the
  preferred path because macOS layouts are configurable.

The new crate is `soil-canvas-macos`: an owner surface that satisfies the seven
host obligations in [`docs/soil-canvas.md`](soil-canvas.md) — one surface, opaque
frames, last-good frame on error, generation-bound input, ordered input with a
bounded queue, named failures, host-owned focus and visibility.

### The parts that are not egui

Three AppKit behaviours decide whether this feels like a launcher or like a
window that takes a second to lose focus:

1. **Non-activating panel.** The launcher must take key input without the
   previously focused app losing "active" status, or the app you return to after
   Escape flashes its menu bar. That means an `NSPanel` with
   `.nonactivatingPanel`, not a plain `NSWindow`.
2. **Every Space, including full screen.** `canJoinAllSpaces` plus
   `fullScreenAuxiliary` in the window's collection behaviour, and a floating
   level. Without both, the launcher simply does not appear over a full-screen
   app.
3. **Collection behaviour changes at runtime.** These are property sets on the
   `NSWindow`, after eframe has created it from a raw window handle.

winit creates the window; it does not expose panel style masks or collection
behaviour. So this is Objective-C FFI.

### FFI and the `unsafe_code` lint

The workspace sets `unsafe_code = "forbid"` in `[workspace.lints.rust]`, but
**workspace lints only bind crates that opt in with `[lints] workspace = true`**,
and today exactly one crate does (`soil-spore`). `cpal-soil` already contains
`unsafe extern "C"` for its `soil_audio` imports and does not opt in.

So a macOS shim does not need a policy change. It needs to follow the existing
precedent: **the platform crate does not opt into workspace lints, and every
unsafe block carries a comment naming the AppKit contract it relies on.** If the
project later wants the lint to be real everywhere, the exception lives in one
crate with one file, not scattered through the runtime.

## 3. Activation

Host policy, in one place, with named failures:

- **Global hotkey.** Carbon `RegisterEventHotKey` is still the path that works
  without an accessibility grant. It is FFI, in the same shim as §2.
- **Compact mode.** With no query, the window is the search bar only; it expands
  on first keystroke and collapses on blur. This is an extent change, so the
  guest sees it as a `state` generation bump, not as a new surface.
- **Pop to root.** A host timer. `soil:time` is monotonic and has no timers by
  design, so this does not belong in the Spore.
- **Escape closes; a second Escape clears.** Order matters; the current
  `ui/launcher.lua` footer already assumes it.

One decision to make explicitly: whether the launcher takes exclusive keyboard
focus while visible. `docs/surface-windowing.md` says overlays are never Spores
because a Spore that can draw a consent prompt can impersonate consent. A
launcher is not a consent prompt, but it is the most convincing phishing surface
on the machine if a non-allowlisted Spore can draw it. Recommendation: the
launcher region keeps the chrome-role surface and Canvas grants exclusive
modality only to a host-allowlisted Spore id — the same `[system] role`
allowlist that `docs/spore-package.md` already defines for `consent`.

## 4. Applications: discovery and launch

Two separate problems that people merge and then regret.

**Discovery.** Add `application` to `soil:search`'s `kind` enum. The host
supplies the inventory from its own scan of `/Applications`, `/System/Applications`
and `~/Applications`, resolving symlinks and Finder aliases, and including the
alternate names and Spotlight keywords that make `iCal` find Calendar. The hit
carries `id` = bundle identifier, `title` = display name, `source` = `apps`.

The Spore gets a locator. It does not get a path it can steer anything at, which
is the same rule `soil:vfs` enforces for files.

**Launch.** One action, `macos.app.open@1`, effect `external`, idempotency
`keyed` (key = bundle id + a caller nonce), approval `none`. Input schema
`{ bundle_id: string, urls: [string] }`. It runs through
`soil-actions-host` to a macOS adapter, so it appears in the audit trail and
inherits the idempotency receipt logic that already exists.

This is why the launcher does not need a `soil:process` grant: it never starts a
process. It asks the host to open an app.

**Icons.** `soil:search.hit` has no icon today, and an app list without icons is
not a launcher. Add `icon: option<string>` as an asset reference
(`asset:sha256:…`) that Canvas resolves at render time. The resolver, the cache
key and the "missing is a named placeholder" rule are already specified in
`spores/docs/content-rendering.md`; this is the first caller that needs it on a
row rather than in a document.

## 5. Clipboard

Every serious launcher copies a result and most keep a history. Two different
things:

- **Write** — `soil:clipboard@0.1` with `write(text)`. Small, safe, and needed
  for "press Enter, copy the answer".
- **History** — host-owned capture with its own opt-in, never a Spore grant. It
  monitors every copy on the machine, which is ambient authority by definition.
  SuperCmd and Floe both explicitly exclude items a password manager marks
  private; that exclusion is a host responsibility and cannot be delegated to a
  Spore that only sees the resulting list.

Ship write in v1. History is a separate decision with a separate privacy story.

## 6. The wall clock

`soil:time@1.0` is monotonic only: "no wall clock, sleep, or timer thread". That
was right when the only consumers were animations. A top bar draws a clock and a
calculator converts `12PM EST to PST`, so a wall clock is now load-bearing.

Recommendation: `soil:clock@1.0` with `now() -> { unix_ms, tz_id, offset_ms }`
and nothing else — no sleep, no timers, no scheduling. It is a read, not a
capability to wait. Keep `soil:time` exactly as it is so existing budget and
epoch logic is untouched.

Currency conversion needs an FX source; do not smuggle it in here. That is
`soil:http` against a host-allowlisted origin, or a host action.

## 7. Deferred host surfaces

Each of these is a real launcher feature that requires authority beyond what a
Spore can hold. Listed so the decision is on record rather than repeatedly
rediscovered.

| Feature | Why it is a host surface | Decision |
|---|---|---|
| Menu-bar item search | Accessibility API; reads another app's menus | defer |
| Selected-text actions | Reads the frontmost app's selection | defer |
| Snippets / text expansion | Global key monitoring plus input injection | defer; highest-risk feature in the category |
| Window tiling of other apps | Accessibility; those windows are not Canvas surfaces | defer, and note it is not a launcher-spore concern |
| Paste into frontmost app | Input injection | defer; copying is v1 |
| Screen capture for glass effects | Screen Recording permission | per-feature opt-in, never implied |

The last one matters for the DragShift Spore: its liquid-glass look refracts the
screen behind the menu, which needs Screen Recording. Without it the glass shows
the wallpaper. That is a graceful fallback and should stay one.

## 8. Sequencing

1. `soil:spore` canonical in `wit/` + `soil-abi` coverage. *(unblocks everything)*
2. `soil:search`: `kind.application` and `hit.icon`.
3. `soil-canvas-macos`: owner surface from `eframe`, non-activating panel,
   collection behaviour, `soil-canvas-input` key path.
4. Host hotkey + compact mode + pop-to-root.
5. `macos.app.open@1` action + inventory adapter.
6. Launcher Spore: the middle from `spores/docs/launcher.md` §4.
7. `soil:clipboard` write.
8. `soil:clock`.

Steps 1–2 are small, testable, and useful without a Mac. Step 3 is the one with
schedule risk, and it is worth building behind the existing `soil:canvas`
contract so the S22 and Android owners keep working unchanged.

## 9. Open questions

- **One presenter crate or a trait?** `s22-canvas-surface` and a macOS presenter
  would both satisfy `soil:canvas`. Extracting the owner-surface trait before the
  second implementation is speculative; after it, obvious. Build the Mac one
  first and extract what actually repeats.
- **Does the hotkey belong to the launcher or to a shell daemon?** If the top bar
  is a Spore that can crash, the hotkey must not crash with it. A tiny host agent
  that owns activation and a Unix socket is the same shape `soil-consent`
  already uses for its broker.
- **Modality scope.** See §3. Recommendation is an allowlisted Spore id, but the
  alternative — Canvas owns the launcher as a built-in like the dock fallback —
  is defensible and strictly safer.
- **Multiple displays.** The launcher opens on the screen with the pointer, or
  the focused window. That is host policy and needs one sentence in the docs so
  two implementers do not disagree.
