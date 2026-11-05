# Soil Canvas: the cross-platform owner surface

Status: **decision + contract, 2026-11-01.** Canvas is the cross-platform owner
for windows, layout, theme, rendering and input. Spores do not submit UI trees.
The canonical contract is [`wit/soil-canvas.wit`](../wit/soil-canvas.wit); the
portable input normalizer is
[`crates/soil-canvas-input`](../crates/soil-canvas-input).

## Decision

- **Canvas owns the UI.** It owns the window/compositor, retained owner model,
  layout, theme, renderer, frame pacing, focus, visibility and input lifecycle.
- **A Spore is logic/data.** It asks the granted Canvas surface to present or to
  receive one normalized input event. It never receives pixels, a framebuffer,
  a DRM/window handle, a path, a socket or a clock.
- **`soil:surface` is deferred, not rejected.** The scene-submission design
  remains a reference for a future where a Spore brings its own UI, or where a
  non-Canvas host exists. It is not on the v1 path.
- **Canvas ships with the host**, not through the Spore Store. `soil:canvas` is
  therefore an explicit host-shell capability, like SystemUI, rather than a
  generic application capability.
- **`soil:view` remains separate**: it is for host-owned web/Spore/retained
  content *inside* Canvas. It is not a second presentation ABI.
- **GPU work is not a second UI channel.** Canvas owns its own rendering. A Spore
  may use the separately granted upstream `wasi:webgpu` for offscreen compute or
  render, but its pixels still have to reach Canvas through a host-owned target;
  it never gets a window or swapchain. A Spore-owned GPU renderer observes
  `soil:canvas.state` for extent, scale and theme, then presents the completed
  texture through `soil:canvas-gpu`; Canvas owns the window and final composite.

Revisit `soil:surface` only when one of these is real:

1. a Spore must draw its own interface, not a Canvas owner view;
2. a host runs without Canvas;
3. an external pixel producer needs a bounded surface-ingress contract.

## Contract

`soil:canvas@1.0.0` has one interface, `surface`, with four calls:

| Call | Meaning |
|---|---|
| `present()` | Render the granted owner surface; the host retains the pixels. Returns `state`. |
| `observe()` | Read the current owner state without presenting. |
| `input(generation, key)` | Deliver one normalized event. A stale generation is refused. |
| `close()` | Release the grant; later calls return `closed`. |

`state` carries `generation`, host-selected `width`/`height`/`scale`, `focused`,
`visible`, and the host-resolved `theme`. The generation increases on
target/navigation/focus/visibility/theme changes, so a late input or repaint
cannot move the owner backwards.

Theme is data, not a callback: mode, background/surface/text/muted/primary/
success/warning/danger/info colors, and the logical type/radius scale. A
Spore-owned renderer can therefore draw with the owner's actual theme instead of
hard-coding colors or trying to sample the framebuffer.

Failure taxonomy: `denied`, `not-ready`, `stale`, `over-budget`, `invalid`,
`closed`, `backend`. There is no placeholder success and no raw error text.

Limits, enforced by the host:

- one granted surface per Spore; the host selects extent and display;
- one `present` and one `input` per invocation, no per-frame callback;
- input is a bounded semantic key, never a platform keycode or device event;
- no pixels, framebuffer, DRM handle, window handle, path, socket or clock.

The host also owns policy that must not leak into WIT: inner/outer or other
extents, lid/display switching, presenter choice, DRM/scanout, and key repeat
timing. `--surface inner|outer`, keypad sockets and BGR frame files are host
implementation details, not contract terms.

## Input

`key` is a closed variant covering phone, remote and media keys, plus text and
editing:

```
up down left right select back home menu info
number(0..=9) star hash call end-call push-to-talk
soft-left soft-right
play-pause rewind fast-forward next previous
volume-up volume-down mute
character(char) backspace delete tab escape
```

Every event also carries `modifiers { shift, control, alt, meta }`. `character`
is one printable Unicode scalar — A-Z, accents and non-ASCII scripts all use the
same path. The host text system is authoritative; `soil-canvas-input::text`
takes the composed character so keyboard layout, dead keys and IME are not
re-invented in Canvas or Soil.

`crates/soil-canvas-input` provides:

- `text(ch, modifiers)` for layout/IME-correct Unicode input;
- Linux evdev mappings for phone/remote semantics and for desktop keyboards
  (`A-Z`, digits, punctuation, Shift, Backspace, Delete, Tab, Escape, arrows);
- Android/Fire TV mappings for both remote keycodes and hardware keyboards;
- macOS virtual-keycode mappings for US-layout printable keys and special keys
  (text remains the preferred path because macOS layouts are configurable);
- held-key tracking, repeat only for directions and text editing, rejection of
  out-of-range digits, and resync that clears held state without inventing
  releases.

The S22 keymap is covered directly: digits, `*`, `#`, phone, soft keys,
app-select, voice/PTT, media and volume. Raw keycodes, device paths and
`{keydown,keyup}` transport details never cross the capability boundary.

## Host obligations

A Canvas host must:

1. create one owner surface and choose its extent from host policy;
2. present only a complete, opaque owner frame of the granted size;
3. keep the last good frame visible on presenter/frame errors;
4. normalize input before calling the guest, and bind it to a generation;
5. keep input delivery ordered with presentation, with a bounded queue;
6. fail with the named `failure` value, never a stale frame as current;
7. treat lid, display, focus and visibility as host policy.

## S22 reference host

The first real host is the S22 native Linux track:

- `soil:canvas` guest → actual `canvas-workspace-ui` owner (`Inbox`, Coherence
  theme, owner navigation);
- inner 480×640 and native 128×128 outer owner layouts;
- `cat-drm-present` remains the sole DRM owner; Soil writes atomic BGR24 frames;
- HAL keypad transport → host normalization → guest `input` → Canvas owner.

The S22 worktree still carries its earlier seven-key normalizer. It must switch
to `crates/soil-canvas-input` before digits, media, call, soft-key or PTT events
reach the owner; the crate already covers the S22 device-tree keymap. That wiring
is the next integration step, not a contract change.

The `.089` inner scanout regression is fixed. The detailed S22 implementation
record lives in [`soil-canvas-s22.md`](soil-canvas-s22.md). Physical acceptance of colour
order, stride, outer readability and both presenters still belongs to the
parent/user; producer correctness is not scanout proof.

The contract is cross-platform: macOS, Linux, FireTV and S22 must implement the
same `soil:canvas` surface rather than fork it. S22 is the proving backend, not
the definition.

## Non-goals for v1

- Spore-authored scene trees or arbitrary widgets;
- custom shaders, render passes or raw GPU buffers;
- more than one window per grant, or guest-chosen geometry/display;
- raw keycodes, pointer capture or access to other windows' input;
- direct scanout, vsync/fence choreography or display enumeration;
- background rendering or per-frame wasm callbacks.

## Open decisions

- When to add pointer/scroll to `soil:canvas`; text, modifiers and soft-keyboard
  characters are now in the contract.
- Whether media/volume keys repeat while held, or stay single-shot.
- Whether the two S22 soft keys are owner-visible actions or host chrome; the
  contract exposes them so the host can decide without a new ABI.
- The exact event vocabulary for `soil:view` targets presented inside Canvas;
  that remains a separate content capability.
