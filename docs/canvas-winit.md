# The native presenter: `canvas-winit`

Status: **implemented.** How a Spore runs as a normal Mac app: a window, a frame
sink, and the host services a Spore asks for — nothing else.

`canvas-winit` is not a host app in the Canvas sense. It owns no scene, no
layout, and no hit testing. It opens a window, runs one component in a wasmtime
store, blits whatever texture the Spore presents into that window, and forwards
the events the window receives. The same Spore runs unchanged under
`soil-webgpu` (one-shot capture), under Canvas's external-surface path, or under
the future web bridge; only the host's implementation of `soil:present` moves.

## What a Spore may ask for

| Contract | What it means |
| --- | --- |
| `soil:present/surface` | `size()` is the window in the Spore's layout space; `scale()` is device pixels per layout pixel; `present(texture)` copies one guest-rendered texture into the window. |
| `soil:present/input` | One ordered stream of `key-input`, `moved`, `button`, `scrolled`. `interactive()` says whether anyone is listening. |
| `soil:present/cursor` | `set(style)` — the Spore says what it is pointing at, the presenter applies it. |
| `soil:spore/catalog` | `entries()`, and `launch(id, surface)`. The catalog is host responsibility, not the Spore's. |
| `soil:terminal-screen/screen` | `snapshot()` returns a bounded cell grid; `write(bytes)`, `scroll(delta)`, `resize(cols, rows)`. The host owns the pty and the emulator. |

Three rules carry most of the design:

- **One ordered event stream.** Keys and pointer actions share a queue, so a
  click that lands between two keystrokes cannot overtake them. Two queues
  would have made that race a matter of luck.
- **Surface space, not device pixels.** `size()` and every pointer coordinate
  are in the same space. The Spore lays out against them and never converts
  between logical and physical pixels itself; the presenter blits whatever
  resolution arrives, scaling only if the Spore could not render at the window's
  size. `scale()` exists so a Spore that opens a framebuffer renders at the
  resolution the window will actually use — a headless window has no display to
  ask, so it has to be told.
- **The terminal is a grid, not a byte stream.** A terminal Spore never sees an
  escape sequence. The host owns the pty and runs `soil-vt` (ghostty's parser),
  hands the Spore a `cols × rows` cell grid with attributes and a cursor, and
  answers the emulator's replies back down the same pty. Selection and mouse
  reporting to the running program are separate contracts and are not part of
  this one.

## Starting a Spore

Compiling a component is the slow part — tens of seconds for a 9.5 MB guest,
where running it is milliseconds. The presenter keeps compiled components in
`~/Library/Caches/dev.fungos.launcher/<sha256>.cwasm`, keyed by the module's
digest:

- a module that has not changed starts from the cache (measured: first frame in
  ~2 s),
- a new build is a new digest, so it compiles once and is cached again.

Until the first frame arrives the window shows the splash: the fungOS mark
masked to the app-icon shape over two lissajous curves, repainting at 30 fps.
It is a loading screen for a real wait, not decoration, and it stops the moment
the Spore presents.

## Running it

```sh
cargo build -p canvas-winit
canvas-winit <module.wasm> [--spores DIR]
canvas-winit                      # a bundle loads Contents/Resources/launcher.wasm
```

Flags for driving a Spore without a human:

| Flag | Effect |
| --- | --- |
| `--capture FILE.rgba` | write the first frame's raw RGBA to a file and exit |
| `--frames N` | stop after N frames (default: only `--capture` bounds it) |
| `--key NAME` | queue one key before the guest starts |
| `--keys TEXT` | queue text as keystrokes, one event per character |
| `--pointer X,Y` | queue a pointer move at a surface-space point |
| `--click X,Y` | queue a move, a press, and a release at that point |
| `--spores DIR` | where to find installed Spores (default `~/Library/Application Support/fungOS/spores`) |

The window title is the Spore's `[spore] title` from its manifest, or its name
title-cased, or the module's file name — an app called "Term" shows "Term", not
`canvas-winit — terminal.wasm`.

## Packaging

The `.app` is a plain bundle: `Contents/MacOS/canvas-winit`,
`Contents/Info.plist`, `Contents/Resources/launcher.wasm`, and a masked
`fungos.icns` (1024² canvas, artwork in the 824 square, 22.37 % corner radius —
the macOS icon grid). It is ad-hoc signed.

A launched Spore is started through Launch Services (`open -n <bundle> --args
<module>`) rather than exec'd: a child that inherits the launcher's session
never becomes the key window, so its clicks and keys go nowhere. A binary run
from a shell has no bundle to hand to Launch Services and re-executes itself
with a null stdin instead.

## Known limits

- **Text selection and mouse reporting** are not implemented; a click in the
  grid folds the sidebar or is ignored. Both need their own contract.
- **Theme**: the splash and the launcher use the brand palette. Deriving colour
  from the Canvas theme needs a source a Spore can read (`soil:theme`, or the
  presenter passing tokens down), which is a contract decision, not a palette.
- **Scale is read at start-up.** Moving a window between a Retina and a
  non-Retina display needs a re-render trigger; the interface can already say
  it.
- **No accessibility tree** is published for guest content yet (`gpu-spores.md`
  describes the two options).
