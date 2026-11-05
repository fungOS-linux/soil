# Terminal surfaces

Status: emulation works (`crates/soil-vt`); guest wiring next.

Soil runs programs. Some of them are terminals. Emulating one is the hard part —
escape sequences, wide clusters, reflow, scrollback, alternate screen — and it is
not a thing to invent. `libghostty-vt` is Ghostty's emulator extracted as a
library, so `soil-vt` wraps it and exposes plain data.

```text
guest stdout ──► soil-vt Session::write ──► Snapshot ──► Canvas scene
guest stdin  ◄── key/mouse input encoded ◄── Canvas input
```

## What `soil-vt` is

- `Session::new(cols, rows, scrollback)` — a grid bound to a size.
- `write(bytes)` — any chunking, including one byte. Partial escapes are the
  emulator's problem, not the caller's.
- `resize(cols, rows)` — content reflows and scrollback survives, which is
  exactly what a resized window needs.
- `snapshot()` — `cols × rows` cells, each a grapheme cluster plus attributes and
  *optional* colours. Empty cells are empty strings, so "blank" and "a space"
  stay distinguishable.

No fonts, no theme, no pixels. A snapshot can cross a WIT boundary unchanged,
which is why it is plain data: the emulator lives in Soil, the drawing lives in
Canvas, and neither needs to know about the other.

## What presenterm needs from us

presenterm is a Rust TUI (ratatui + crossterm). Compiled to `wasm32-wasip1` it
wants things WASI does not provide:

| It asks for | WASI gives | What Soil does |
|---|---|---|
| a tty on stdin/stdout | a pipe | we *are* the tty: stdout bytes feed the VT, keys are written to stdin |
| terminal size | nothing | answer the size query from the surface, and send a resize when the window changes |
| cursor position / colour queries | nothing | answer from the `Snapshot` (DSR, DA, and colour queries the emulator already tracks) |
| raw mode / termios | ENOTTY | report success for the calls that are already true of a pipe (raw mode is the default state of a pty) |
| alternate screen, truecolor | plain bytes | free: they are escape sequences, and the emulator handles them |

The shim is therefore small and honest: a handful of `isatty`/`ioctl` answers,
not a fake kernel.

## Input

`libghostty-vt` also encodes input (`key`, `mouse`, `paste`, `focus`), which is
what makes the loop symmetric: Canvas delivers normalized events, Soil encodes
them into the bytes the running program expects, including the kitty keyboard
protocol if the program enabled it. A Spore never sees terminal escape
sequences and never produces them.

## Rendering

`Snapshot` → a Canvas module draws a grid: one text node per run of cells sharing
a style, a rect per background run, and the cursor as Canvas's own overlay rather
than a glyph. Font choice, cell metrics, and the theme live in the Canvas theme
tokens, so a terminal in fungOS looks like the rest of the system and reflows with
it. Wide clusters consume one cell and mark their tail.

## Build note

`libghostty-vt-sys` builds Ghostty's C sources with **Zig 0.15.2** — not newer.
The toolchain is at `~/.local/zig` in this environment; put it on `PATH` when
building `soil-vt`. A pinned toolchain belongs in the image once this ships.

## Workspace note

`soil-runtime` and `soil-android` are excluded from the workspace: they depend on
sibling worktrees that are not present in this checkout
(`fpl/gfx .worktrees/s22-software`, `reversal .worktrees/s22-gpu-ui`). That is
someone's work in progress, not something to delete; re-include both when the
paths exist.

## Showing a diagnostic

A failed Lua module, a boot log, a traceback — these are terminal output, not
paragraphs. `soil-terminal` can therefore be pointed at text instead of a
program:

```sh
soil-terminal --title "Canvas · field-notes" --text /tmp/canvas-error.txt
soil-terminal --title "soil" --text -        # from stdin
```

The text is fed through the same emulator a program writes to, so colour codes
and cursor movement in a traceback render the way the tool that produced them
intended, and an error looks the same whether it is on a wall or in a log.

A host that fails to load something should hand the failure to this rather than
inventing an error panel: one place to read errors, one renderer to trust.

## Colour, weight, and the kitty protocol

| Question | Answer |
|---|---|
| 24-bit colour | Yes. `libghostty-vt` reports `Some(rgb)` per cell for both foreground and background; the renderer honours it and falls back to the theme only when a cell asks for nothing. |
| Bold | Yes, and as a real face. `cell.bold` selects the Bold cut of the same family rather than smearing the regular one, which is what makes fake bold unreadable at small sizes. |
| Underline | Yes, drawn across the whole cell so underlined blanks stay visible. |
| Italic | Reported by the emulator, not drawn yet: needs the italic face loaded alongside regular and bold. |
| Kitty keyboard | **Not negotiated yet.** The emulator tracks the flags (`kitty_keyboard_flags()`), and `libghostty-vt::key::Encoder` will encode against them — including `set_options_from_terminal(&terminal)`, which is the call that makes the protocol automatic. The current encoder is a portable subset (arrows, home/end, ctrl-keys, alt-prefix), which every TUI understands but which cannot negotiate. |
| Kitty graphics | No. Out of scope for a text grid; it needs an image pipeline. |

Switching input to the library encoder is the next step and it removes the
hand-rolled table entirely: one source of truth for how a key becomes bytes,
including the kitty, modifyOtherKeys, and macOS-option-as-alt behaviours.
