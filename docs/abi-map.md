# Soil ABI map

Status: design inventory, 2026-10-27. This document maps the host capabilities a
Spore may import and the ones it must not. It does not grant anything; a Spore
gets a capability only when its signed manifest, the Mycelium grant, and host
policy intersect.

The runtime contract already lives in `canvas-model-host` (`wasmtime 34`,
core no-import scenes + component-model Soil S1). The canonical WIT packages
should move here once the runtime crate is extracted; until then this is the
review map for what exists and what is missing.

## Rules

1. **Transport in Soil, protocol in the Spore.** A Spore never receives a
   socket, path, device handle, token, lease, or display handle it can forge.
2. **No ambient authority.** No filesystem, network, clock, environment, or
   device access unless a named capability is linked and granted.
3. **Bounded and polled.** No callbacks into wasm. Every call has a deadline,
   byte/item limit, and typed error.
4. **Soil owns the platform object.** GPU device, camera device, audio device,
   window, radio, and bus connection stay in the host.
5. **No second ACL.** Mycelium owns identity/grants; Unibus enforces on the
   bus; Soil enforces capability linking. A connector or Spore never mints
   authority.
6. **Honest absence.** Unsupported or denied is a named error, never a
   placeholder success.

## Capability matrix

| WIT package | Host object | Grant shape | v1? | Notes |
|---|---|---|---|---|
| `soil:surface@1` | window / compositor / seat | `soil:surface` | yes | Spore submits a bounded retained scene; polls normalized pointer/key/focus/resize/tick. No raw keycodes, display ids, or window creation. |
| `soil:gpu@1` | wgpu/Metal/Vulkan/GLES device | `soil:gpu` | yes | Bounded draw list with CID meshes. No shaders, pipelines, raw buffers, or compute. |
| `soil:audio@1` | audio sink | `soil:audio.playback` | yes | Bounded PCM push, credit, underruns. Sink is host policy. |
| `soil:mic@1` | audio capture | `soil:mic.capture` | yes | Bounded PCM chunks. Visible capture indicator; no background capture. Separate from playback so muting output never implies mic permission. |
| `soil:camera@1` | camera capture | `soil:camera.capture` | yes | Still/low-rate frames only. Full-rate video belongs on `soil:stream`/producer path. No device paths or raw controls. |
| `soil:input@1` | gamepad / remote / global input | `soil:input` | later | Surface already owns windowed pointer/key. This is for controllers and media keys when no surface is focused. |
| `soil:ble@1` | BLE radio | `soil:ble.observe`, `soil:ble.connect`, `soil:ble.write` | yes | Eight verbs, Soil-issued handles, signed lease for the radio. |
| `soil:stream@1` | Unibus stream | `soil:stream` plus kind/schema grants | yes | Bounded subscription resource; read/write/credit; no endpoints, producer ids, or tokens. |
| `soil:api@1` | authenticated upstream API | `soil:api` plus service/operation grant | yes | Host-mediated request/response. Soil holds the credential and mints/brokers delegated tokens; the Spore never sees them. |
| `soil:storage@1` | content-addressed assets | `soil:storage.read`, optional `soil:storage.write` | yes | CIDs, never paths. Read-only cache by default. |
| `soil:time@1` | host monotonic clock | `soil:time` | yes | Monotonic milliseconds and host ticks only. No wall clock, sleep, or timer thread. |
| `soil:notify@1` | host notification UI | `soil:notify` | later | Rate-limited, user-visible. Never a background toast loop. |
| `soil:display@1` | display enumeration/placement | — | no | Soil owns displays. Spore asks for a surface purpose, not a display id. |
| `soil:net@1` | raw sockets/DNS | — | no | Use `soil:api` or `soil:stream`. No ambient HTTP, redirects, or arbitrary hosts. |
| `soil:process@1` | process spawn | — | no | A Spore is not a process manager. |
| `soil:secrets@1` | secret storage | — | no | Host injects credentials for `soil:api`; wasm never reads secrets. |
| `soil:compute@1` | GPU compute | — | no (v1) | Requires a different buffer/kernel/queue model. Add only with a concrete workload and hard budgets. |

`soil:isochrone` is intentionally absent. Isochrone is an application/service,
not a host capability. It should expose its API through `soil:api`, its media
through `soil:stream`, and optionally use `soil:audio`/`soil:gpu`. Baking an
application into the host ABI would break the capability model and make every
other app depend on it.

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
// Only for input that is not tied to a `soil:surface` window.
// No raw keycodes; use named buttons/axes.
package soil:input@1.0.0;

interface input {
  record event {
    kind: string,          // button | axis | text | media
    control: string,       // "a", "dpad-left", "play-pause"
    value: f32,
    generation: u64,
    soil-time-ms: u64,
  }

  enum error { denied, not-ready, over-budget, closed }

  open: func() -> result<input, error>;
  poll: func(timeout-ms: u32) -> result<option<event>, error>;
  close: func() -> result<_, error>;
}
```

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

Any unsupported row is a named denial, not an empty success.

## Priorities

1. **Already in S1/runtime:** `surface` (design), `gpu` (WIT), `audio` playback
   (WIT), `ble` (component + mock), `stream` (design).
2. **Next, highest value:** `api`, `storage`, `time`, `mic`, `camera`.
3. **After that:** `input`, `notify`.
4. **Only with a concrete workload:** `compute`, platform sensors, telephony,
   raw display control.

## Non-goals for v1

- raw sockets or arbitrary HTTP
- filesystem paths or directory traversal
- shader/pipeline submission
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
- Whether `soil:time` is a grant or an always-available read-only primitive.
- Whether `soil:api` is the stable public name or a typed per-service package
  (`marbles:views`, `isochrone:...`) is preferred at the call site.
