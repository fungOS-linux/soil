# `soil:surface-import`

Status: **contract + host seam + Canvas layer, 2026-11-05.**

`soil:surface-import@0.1.0` represents an imported host-owned surface:

```text
VST3 editor
Jellyfin/YouTube video
remote desktop
browser view
another Spore's render target
retained host scene
```

A Spore gets an opaque handle, metadata, damage/frame events, and normalized
input. It never gets a native window handle, `wgpu` texture, decoder handle, or
per-frame pixel buffer.

## Surface

```wit
open(source-id) -> surface
surface.info() -> info
surface.next(timeout-ms) -> option<event>
surface.input(event)
surface.close()
```

```text
info { kind, width, height, scale, color-space, alpha }
event { resized, frame, damaged, idle, closed }
frame { id, pts-ms, damage, keyframe }
input { pointer, button, scroll, key, text, focus }
```

`frame` carries metadata only. The host owns the texture/frame. The Spore can
control placement, focus, and input; it does not own pixels.

## Canvas composition

`crates/s22-canvas-surface` now accepts a host-only `SurfaceLayer`:

```rust
canvas.set_surface_layer(Some(SurfaceLayer {
    rgba,
    width,
    height,
    x,
    y,
}));
```

The layer is uploaded as an egui texture and painted at `Order::Foreground`.
It is a software baseline for host adapters. Native adapters should replace the
RGBA upload with a shared texture (`IOSurface`, `dmabuf`, D3D shared texture)
and keep the same Canvas composition point.

## VST3

A VST3 editor is a native `NSView`/`HWND`/X11/Wayland view. It must run in a
native, preferably out-of-process, VST3 host:

```text
Soil host -> VST3 host process -> plugin editor
          -> SurfaceSession
          -> Canvas SurfaceLayer / shared texture
```

The Spore controls plugin parameters and state through a host API. It does not
load the plugin, read its window, or receive the view pointer.

For remote VST3 UI, the host streams damage frames and forwards normalized input.
A static editor sends no frames until it changes.

## Other sources

- **Jellyfin**: host adapter resolves item + playback info, then feeds
  `soil:media` or a `soil:surface-import` video surface.
- **YouTube**: host adapter owns extraction/policy; never a guest URL.
- **CID/Unibus**: host resolves the content and transport; CID is a locator,
  not authority.
- **Another Spore**: host runs it separately and exposes its rendered target as
  a surface. No Spore-to-Spore linkage.

## Current host

`crates/soil-surface-import-host` implements the resource table and a `SurfaceSource`
trait. It tests lifecycle, events, and normalized input with a fixture source.
Native window import, VST3 hosting, remote transport, and GPU texture import are
host adapters, not guest ABI.
