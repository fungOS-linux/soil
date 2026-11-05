# `soil:media`

Status: **contract + host seam + image readback, 2026-11-05.**

`soil:media@0.1.0` is the host-mediated media surface. It is deliberately three
interfaces, not one “give me bytes” API:

| Interface | Grant | Purpose |
|---|---|---|
| `playback` | `soil:media` | host demux/decode/present; Spore controls playback |
| `encoded` | `soil:media-encoded` | explicit encoded chunks for recording/remux/transcode |
| `image` | `soil:media` | one decoded image with bounded RGBA readback |

## Playback

```wit
open(source-id, options) -> playback
playback.status() -> status
playback.next(timeout-ms) -> option<event>
playback.play() / pause() / seek(ms) / close()
```

Events are timeline-driven:

```text
opened(format)
buffering
playing
paused
position(ms)
format-changed(format)
discontinuity
ended
```

There is no fixed output FPS. The host decodes and presents at the media
timeline. A 24fps film produces frames because its PTS says so; a static scene
does not produce a frame every 1/24s.

The host owns:

- source resolution
- Unibus/HTTP/VFS transport
- credentials
- demux
- video decode
- audio decode and A/V clock
- Canvas/video-layer presentation

## Encoded

`encoded` is separate on purpose. It returns bounded chunks with:

```text
data
pts-ms
duration-ms
keyframe
codec-config
discontinuity
```

Use it for recording, remuxing, transcoding, inspection, or a codec the host
cannot present. It does not imply playback authority.

## Images

```wit
open(source-id) -> image
image.info() -> { width, height, media-type }
image.rgba(max-width, max-height) -> { width, height, data }
```

The host decodes PNG/JPEG/GIF/WebP, bounds source bytes, downsamples to the
requested dimensions, and returns tightly packed RGBA8. Returned dimensions are
authoritative. This is a one-shot readback path, not a per-frame video API.

## Source adapters

The WIT uses a host-reviewed `source-id`, never a URL or credential. A host
adapter resolves it:

```text
source-id
  -> local file / VFS path
  -> Unibus/CID content
  -> Jellyfin item
  -> YouTube resolver
  -> remote stream
```

Jellyfin, YouTube, and Unibus are not WIT packages. They are host source
adapters. Jellyfin owns API keys and playback-info negotiation; a YouTube
adapter must own its own policy and extraction; Unibus owns peer routing and
content fetch. The Spore sees only the source id and controls.

CID is a locator. A host may resolve a CID through VFS/Unibus/storage, but the
CID itself grants nothing and is not a second access check.

## Current host

`crates/soil-media-host` implements the resource table and a `MediaSource`
trait for host adapters. It includes image decode and tests for playback events,
encoded chunks, and bounded image readback. A production decoder/presenter is
still a host adapter, not part of the guest ABI.

## Fixture

[`spores/media-probe`](../spores/media-probe) exercises playback, encoded chunks, and image readback against a host `MediaSource` fixture.
