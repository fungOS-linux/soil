# `soil-layout`: host-owned UI geometry

Status: **initial host layout crate, 2026-11-04.** Canonical source is
[`crates/soil-layout`](../crates/soil-layout).

Canvas owns pixels, fonts and text shaping. A guest or Lua module describes a
bounded tree; the host measures text and resolves the tree with
[Taffy](https://crates.io/crates/taffy). The result is a flat list of absolute
rectangles. This keeps layout deterministic and keeps font metrics,
platform metrics and rendering inside the host.

## Contract

```rust
pub trait TextMeasurer {
    fn measure(&mut self, text: &str, style: &TextStyle, max_width: Option<f32>) -> Size;
}

pub fn solve(
    root: &Node,
    available: Size,
    measurer: &mut impl TextMeasurer,
) -> Result<LayoutResult, LayoutError>;
```

`Node` is a serializable tree:

- `container`: row/column direction, gap, padding, fixed/min/max size,
  flex-grow/shrink and alignment;
- `text`: text plus `TextStyle { size, line_height, wrap }`, measured by the host;
- `rule` and `spacer`: leaves with layout constraints.

Every node has a stable `id` and an optional child list. `solve` rejects empty
trees, duplicate ids, non-finite/negative sizes and invalid text metrics.

The host fills an unset root width/height with the available viewport. The
result contains the root rectangle and a pre-order list of
`PlacedNode { id, parent, rect, kind }`.

## Why Taffy

Taffy supplies the flex/grid/block solver, min/max constraints and measure
callbacks. It is the engine behind Zed's GPUI, but `soil-layout` uses it
directly instead of adopting an application framework.

## Text measurement

`soil-layout` deliberately has no font dependency. Canvas implements
`TextMeasurer` with the same `cosmic-text` font system it uses to draw. A
different host must provide equivalent metrics; a guest never sees a font,
glyph atlas or pixel buffer.

## Not an ABI yet

This is a host crate first. A `soil:layout` WIT capability and a guest SDK should
land only with a real Wasm consumer. Canvas's Lua binding is the first consumer:
it passes the same serializable tree and receives rectangles.

## Tests

The crate tests cover padding/gap, flex-grow row allocation, text wrapping,
duplicate ids, invalid sizes and serde round trips. Run:

```sh
cargo test -p soil-layout
```
