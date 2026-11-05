# Soil ABI map

Status: design inventory, 2026-10-27. This document maps the host capabilities a
Spore may import and the ones it must not. It does not grant anything; a Spore
gets a capability only when its signed manifest, the Mycelium grant, and host
policy intersect.

The canonical contracts for `soil:view`, `soil:time`, `soil:storage`,
`soil:vfs`, `soil:index`, `soil:ble` and `soil:canvas` live in [`wit/`](../wit)
and are enforced by `crates/soil-abi`. The BLE host, the COLMI ring Spore, the S22 runtime, the
Canvas input normalizer and the Canvas decision record also live here. The remaining runtime
contract still lives in `canvas-model-host` (`wasmtime 50`, core no-import scenes
+ component-model Soil S1) until that crate is extracted; this document is the
review map for what else exists and what is missing.

## Rules

1. **Transport in Soil, protocol in the Spore.** A Spore never receives a
   socket, path, device handle, token, lease, or display handle it can forge.
2. **No ambient authority.** No filesystem, network, clock, environment, or
   device access unless a named capability is linked and granted.
3. **Bounded and polled.** No callbacks into wasm. Every call has a deadline,
   byte/item limit, and typed error.
4. **Soil owns the platform object.** GPU device, camera device, audio device,
   window, radio, bus connection, VFS namespace and derived index stay in the host.
5. **No second ACL.** Mycelium owns identity/grants; Unibus enforces on the
   bus; Soil enforces capability linking. A connector or Spore never mints
   authority.
6. **Honest absence.** Unsupported or denied is a named error, never a
   placeholder success.

## Capability matrix

| WIT package | Host object | Grant shape | v1? | Notes |
|---|---|---|---|---|
| `soil:canvas@1` | Canvas owner surface (window / layout / renderer) | `soil:canvas` | yes | Spore presents or navigates the host-owned Canvas surface. Typed state, generation-bound input, no pixels or display handles. Canonical WIT: [`wit/soil-canvas.wit`](../wit/soil-canvas.wit). |
| `soil:surface@1` | window / compositor / seat | `soil:surface` | **deferred** | Spore submits a bounded retained scene; only if a Spore or non-Canvas host ever needs its own UI. Not on the v1 path. |
| `wasi:webgpu@0.3.0-rc.2` | wgpu-core device | link `wasi:webgpu` | yes | Upstream WIT, no native handles. Host: [`crates/soil-webgpu`](../crates/soil-webgpu). Presentation stays Canvas-owned. |
| `wasi:audio@0.1.0` | audio sink | link `wasi:audio` | draft | Soil-maintained PCM sink; cpal host; no callbacks or device handles. [`docs/wasi-audio.md`](wasi-audio.md). |
| `soil:mic@1` | audio capture | `soil:mic.capture` | yes | Bounded PCM chunks. Visible capture indicator; no background capture. Separate from playback so muting output never implies mic permission. |
| `soil:camera@1` | camera capture | `soil:camera.capture` | yes | Still/low-rate frames only. Full-rate video belongs on `soil:stream`/producer path. No device paths or raw controls. |
| `soil:input@1` | shared normalized input types | — | yes | Key, modifiers, pointer, scroll, text and focus types shared by Canvas, Terminal and imported surfaces. Types only; each surface keeps its own presentation grant. Canonical WIT: [`wit/soil-input.wit`](../wit/soil-input.wit). |
| `soil:ble@1` | BLE radio | `soil:ble.observe`, `soil:ble.connect`, `soil:ble.write` | yes | Eight verbs, Soil-issued handles, typed errors, local lease (signed lease pending). Canonical WIT: [`wit/soil-ble.wit`](../wit/soil-ble.wit); host: [`crates/soil-ble-host`](../crates/soil-ble-host). |
| `soil:stream@1` | Unibus stream | `soil:stream` plus kind/schema grants | yes | Bounded subscription resource; read/write/credit; no endpoints, producer ids, or tokens. |
| `soil:view@1` | host-owned web/Spore/retained view | `soil:view`, optional `soil:view.capture` | yes | Target ids only; bounded open/act/observe/capture. Host owns the browser/Spore engine, pixels and capture buffer. Canonical WIT: [`wit/soil-view.wit`](../wit/soil-view.wit). |
| `soil:http@0.1` | bounded host-mediated HTTP | `soil:http` | draft | GET/HEAD, host origin allowlist, bounded bodies, opaque credential handles. Direct transport runs; Iroh/Mycelium routing is a host transport seam. Canonical WIT: [`wit/soil-http.wit`](../wit/soil-http.wit). |
| `soil:api@1` | authenticated upstream API | `soil:api` plus service/operation grant | yes | Host-mediated request/response. Soil holds the credential and mints/brokers delegated tokens; the Spore never sees them. |
| `soil:storage@1` | content-addressed assets | `soil:storage.read`, optional `soil:storage.write` | yes | CIDs, never paths. Read-only cache by default. Canonical WIT: [`wit/soil-storage.wit`](../wit/soil-storage.wit). |
| `soil:vfs@1` | workspace namespace and bounded reads | `soil:vfs` | yes | Canonical workspace-relative paths, `ContentId`, paged listing and range reads. No host paths or providers. Canonical WIT: [`wit/soil-vfs.wit`](../wit/soil-vfs.wit). |
| `soil:appdata@1` | private app storage | `soil:appdata` | yes | Local and per-user/per-app synced key-value storage. Opaque bytes, quota-bounded, not indexed or shareable. Canonical WIT: [`wit/soil-appdata.wit`](../wit/soil-appdata.wit). |
| `soil:records@0.1` | durable records | `soil:records` read/write/sync | draft | Schema-tagged entries, tombstones, scan/changes/apply. Host adapter: [`crates/soil-records-host`](../crates/soil-records-host); guest SDK: [`crates/soil-sdk-records`](../crates/soil-sdk-records). |
| `soil:index@1.1` | derived browse/search index + KNN | `soil:index` | yes | Rebuildable, generationed, returns paths and snippets; fast direct-child traversal; explicit KNN when the host has embeddings. Never authority. Canonical WIT: [`wit/soil-index.wit`](../wit/soil-index.wit). |
| `soil:vfs-mut@0.1` | bounded VFS mutation | `soil:vfs-mut` | draft | Separate write/remove/mkdir/rmdir/rename grant. Paths stay workspace-relative; no host handles. Canonical WIT: [`wit/soil-vfs-mut.wit`](../wit/soil-vfs-mut.wit). |
| `soil:graph@0.1` | graph queries | `soil:graph` | draft | Read-only outgoing/incoming edge and reference queries over approved records books. Canonical WIT: [`wit/soil-graph.wit`](../wit/soil-graph.wit). |
| `soil:media@0.1` | host-mediated media | `soil:media`, optional `soil:media-encoded` | draft | PTS/event-driven playback, encoded chunks, and bounded image readback. Source adapters (Unibus/CID, Jellyfin, YouTube) stay host-owned. Canonical WIT: [`wit/soil-media.wit`](../wit/soil-media.wit). |
| `soil:surface-import@0.1` | host-owned imported surfaces | `soil:surface-import` | draft | VST3/remote/media/browser/Spore surfaces as opaque handles with damage/frame events and normalized input. Canvas composites host-owned pixels; no per-frame guest copies. Canonical WIT: [`wit/soil-surface-import.wit`](../wit/soil-surface-import.wit). |
| `soil:inference@0.2` | host-mediated model inference | `soil:inference` | draft | Models, tool calls, attachments, non-streaming completion, and a polled SSE `completion-stream` resource. Host owns provider, endpoint, Geode credential, scrubbing, and transport. Canonical WIT: [`wit/soil-inference.wit`](../wit/soil-inference.wit). |
| `soil:search@0.3` | global search + typed mentions | `soil:search` | draft | Host-composed files + records + future actions, plus `@` suggestions with stable locators. Locators only, never authority. Host: [`crates/soil-search-host`](../crates/soil-search-host). |
| `soil:time@1` | host monotonic clock | `soil:time` | yes | Monotonic milliseconds and host ticks only. No wall clock, sleep, or timer thread. Canonical WIT: [`wit/soil-time.wit`](../wit/soil-time.wit). |
| `soil:notify@1.1` | host notification broker / UI + ringer | `soil:notify` plus separate `history`, `cleanup`, `focus`, `ringer` grants | draft | Durable envelopes, per-device inbox state, host focus, opt-in history, scoped cleanup, and the one-device ringer answer claim. Canonical WIT: [`wit/soil-notify.wit`](../wit/soil-notify.wit); host: [`crates/soil-notify-host`](../crates/soil-notify-host). |
| `fungos:actions@1` | host action bus + presentation catalog | exact action id/version grants | draft | Granted catalog, bounded invocation, approval and idempotency receipts. Canvas presentation uses `fungos:actions/catalog`; execution still goes through `fungos:actions/actions`. Canonical WIT: [`wit/fungos-actions.wit`](../wit/fungos-actions.wit). |
| `soil:display@1` | display enumeration/placement | — | no | Soil owns displays. Spore asks for a surface purpose, not a display id. |
| `soil:net@1` | raw sockets/DNS | — | no | Use `soil:http`, `soil:api`, or `soil:stream`. No ambient sockets, DNS, redirects, or arbitrary hosts. |
| `soil:process@1` | process spawn | — | no | A Spore is not a process manager. |
| `soil:secrets@1` | secret storage | — | no | Host injects credentials for `soil:api`; wasm never reads secrets. |
| `soil:compute@1` | GPU compute | — | no (v1) | Requires a different buffer/kernel/queue model. Add only with a concrete workload and hard budgets. |

`soil:isochrone` is intentionally absent. Isochrone is an application/service,
not a host capability. It should expose its API through `soil:api`, its media
through `soil:stream`, and optionally use `soil:audio`/`soil:gpu`. Baking an
application into the host ABI would break the capability model and make every
other app depend on it.

`soil:canvas` is the explicit exception: Canvas is the fleet's cross-platform
shell, not a Spore. It ships with the host, owns the window/compositor/layout/
renderer, and is the only way a Spore presents UI. `soil:surface` stays deferred
until a Spore must draw its own interface or a host runs without Canvas. See
[`docs/soil-canvas.md`](soil-canvas.md).

## Contract sketches

### `soil:camera@1`

```wit
// Limits: one camera per Spore; still/low-rate only; max 1 frame in flight;
// max 8 MiB per encoded frame; no device enumeration or controls.
package soil:camera@1.0.0;

interface camera {
  record format {
    width: u32,
    height: u32,
    pixel-format: string,
  }

  record frame {
    sequence: u64,
    soil-time-ms: u64,
    format: format,
    data: list<u8>,
  }

  enum error {
    denied,
    not-found,
    not-ready,
    over-budget,
    invalid,
    closed,
  }

  open: func(purpose: string) -> result<camera, error>;
  // resource or polled handle; one frame at a time, caller copies.
  next-frame: func(timeout-ms: u32) -> result<option<frame>, error>;
  close: func() -> result<_, error>;
}
```

A full-rate camera should not copy every frame through wasm. Use a producer
path on `soil:stream` or `soil:surface` and let Soil own the buffer.

The browser/finder design built on `soil:vfs` and `soil:index` is in
[`docs/file-browser.md`](file-browser.md).

### `soil:mic@1`

```wit
// Limits: one capture stream per Spore; 16 kHz..48 kHz; mono/stereo;
// max 8192 frames per call; host-selected device.
package soil:mic@1.0.0;

interface mic {
  record pcm {
    sample-rate: u32,
    channels: u8,
    frames: u32,
    samples: list<f32>,
  }

  enum error {
    denied,
    not-ready,
    over-budget,
    invalid,
    closed,
  }

  open: func() -> result<mic, error>;
  read: func(max-frames: u32, timeout-ms: u32) -> result<option<pcm>, error>;
  close: func() -> result<_, error>;
}
```

Capture is separate from `soil:audio` playback so grants and user indicators
are independent.

### `soil:input@1`

```wit
package soil:input@1.0.0;

interface types {
  // Shared key, modifiers, button, point, scroll, key-event and input types.
  // Canvas, Terminal and soil:surface-import use the same vocabulary.
}
```

This package contains types only. It exposes no grant and no callable
capability. Each surface keeps its own grant and verbs.

### `soil:api@1`

```wit
// Host-mediated upstream call. The Spore names a target, never a URL/token.
package soil:api@1.0.0;

interface api {
  record request {
    target: string,        // an id from the reviewed target index
    content-type: string,
    body: list<u8>,
  }

  enum error {
    denied,
    unknown-target,
    not-ready,
    over-budget,
    upstream,
    invalid,
  }

  call: func(request: request) -> result<list<u8>, error>;
}
```

Soil resolves the target through its index, checks the Mycelium grant, attaches
the credential/token, refuses redirects and unbounded bodies, and returns only
the response body. For external OIDC APIs Soil uses token exchange; for
fleet-internal APIs it can mint a short-lived delegated JWT with
`sub=user`, `act=spore`, `aud=service`, and request binding. The Spore never
sees that token.

### `soil:storage@1`

Canonical contract: [`wit/soil-storage.wit`](../wit/soil-storage.wit). The sketch
below is retained for review history; the WIT file wins.

```wit
// CIDs only; no paths, directory listing, or arbitrary URLs.
package soil:storage@1.0.0;

interface storage {
  enum error { denied, not-found, over-budget, invalid, io }

  read: func(cid: string, max-bytes: u32) -> result<list<u8>, error>;
  // write is a separate optional grant and a bounded cache, never the asset store.
  write: func(cid: string, data: list<u8>) -> result<_, error>;
}
```

### `soil:time@1`

Canonical contract: [`wit/soil-time.wit`](../wit/soil-time.wit). The sketch below
is retained for review history; the WIT file wins. The grant question is settled:
`soil:time` is a grant, matching the no-ambient-authority rule, so both functions
return `denied` without it.

```wit
// Monotonic only. No wall clock, timezone, or sleeping thread.
package soil:time@1.0.0;

interface time {
  enum error { denied, not-ready }
  monotonic-ms: func() -> result<u64, error>;
  tick: func(timeout-ms: u32) -> result<option<u64>, error>;
}
```

## Host backends

| Capability | macOS | Linux | Android / FireTV | tvOS | Browser |
|---|---|---|---|---|---|
| surface | AppKit + iced/wgpu | Smithay/Wayland + wgpu | GLSurfaceView + JNI | UIKit/CoreGraphics | Canvas/DOM + rAF |
| gpu | Metal/wgpu | Vulkan/EGL/wgpu | GLES/Vulkan | CoreGraphics/limited | WebGPU; no silent WebGL fallback |
| audio out | CoreAudio | PipeWire/ALSA | AudioTrack | AVAudioSession | Web Audio |
| mic | AVFoundation | PipeWire/ALSA | AudioRecord | AVAudioSession | getUserMedia + AudioWorklet |
| camera | AVFoundation | V4L2/PipeWire | Camera2 | AVFoundation | getUserMedia |
| input | IOKit/GameController | evdev/libinput | Android input | UIKit focus | DOM/gamepad |
| ble | CoreBluetooth | BlueZ | Android BLE | unavailable | Web Bluetooth (Chromium only) |
| stream | Unibus/QUIC | Unibus/QUIC | Unibus/QUIC | Unibus/QUIC | WebTransport, explicit WebSocket fallback |
| api | host HTTP + token exchange | same | same | same | BFF/host-mediated; no token in wasm |
| storage | local CID cache | local CID cache | app cache | bundle only | OPFS/Cache Storage via host |
| time | `mach_absolute_time` | `CLOCK_MONOTONIC` | `SystemClock.elapsedRealtime` | `mach_absolute_time` | `performance.now` |

Any unsupported row is a named denial, not an empty success.

## Priorities

1. **Canonical here:** `view` (WIT + state machine), `time` (WIT), `storage`
   (WIT), `ble` (WIT + wasmtime/btleplug host + COLMI ring Spore), `canvas`
   (WIT + portable input normalizer + S22 reference host).
2. **Upstream and wired here:** `wasi:webgpu@0.3.0-rc.2` (host + compute proof,
   [`docs/soil-webgpu.md`](soil-webgpu.md)) and draft `wasi:audio@0.1.0`
   (host + tone proof, [`docs/wasi-audio.md`](wasi-audio.md)). `stream` is
   design. `soil:surface` is deferred, not scheduled.
3. **Next, highest value:** build a small read-only file-browser Spore on the
   now-implemented `soil:vfs`/`soil:index` host adapter, then wire
   `soil:canvas-input` into the S22/Fire TV hosts,
   finish the BLE gaps (signed lease, DIS `ring-id`, Unibus publish, Android
   JNI), then the host adapters and denial fixtures for `time` and `storage`,
   then `api`, `mic`, `camera`. `audio` waits for a concrete Spore that plays
   something.
4. **After that:** `notify`.
5. **Only with a concrete workload:** `surface` (if a Spore/non-Canvas host
   needs its own UI), `compute`, platform sensors, telephony, raw display
   control.

## Non-goals for v1

- raw sockets, DNS, or ambient HTTP; use bounded `soil:http` or `soil:api`
- filesystem paths or directory traversal
- raw keycodes or display enumeration
- process spawning
- per-frame wasm callbacks
- application-specific packages such as Isochrone
- secrets in wasm memory
- a second ACL or identity system

## Open decisions

- Whether `soil:mic` stays separate from `soil:audio` or becomes
  `soil:audio.capture`; separate is safer for grants and privacy.
- Whether `soil:camera` ever carries full-rate video, or only still/low-rate
  frames with `soil:stream` for video.
- What the `soil:storage` write budget and eviction policy are. The contract
  bounds writes per call and leaves total cache capacity to host policy.
- Whether `soil:api` is the stable public name or a typed per-service package
  (`marbles:views`, `isochrone:...`) is preferred at the call site.

Resolved: `soil:time` is a grant, not an always-available primitive. Both
`monotonic-ms` and `tick` return `denied` without `soil:time`; hosts must not
silently provide an ambient clock.
