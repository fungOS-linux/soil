# Capture: microphone, camera and screencast

Status: **canonical contracts, microphone and camera host adapters, WIT linker pending.**

`soil:mic@1.0.0`, `soil:camera@1.0.0` and `soil:screencast@0.1.0` are the three
guest-facing capture contracts. They are deliberately separate because they
produce different data and carry different privacy risks:

| Capability | Grant | Guest receives | Never receives |
|---|---|---|---|
| `soil:mic` | `soil:mic.capture` | bounded interleaved PCM chunks | device handles, callbacks |
| `soil:camera` | `soil:camera.capture` | still/low-rate frame copies | camera controls, full-rate video |
| `soil:screencast` | `soil:screencast.capture` | an opaque session and surface alias | display ids, window handles, pixels |

Canonical WIT:

- [`wit/soil-mic.wit`](../wit/soil-mic.wit)
- [`wit/soil-camera.wit`](../wit/soil-camera.wit)
- [`wit/soil-screencast.wit`](../wit/soil-screencast.wit)

## Why three contracts

Microphone capture is a PCM stream. It is useful to agents, recorders and voice
apps, and its grant must be independent from audio playback so muting output
never implies microphone permission.

Camera capture is a low-rate frame path. A preview or still is small enough to
copy through wasm; full-rate video is not. Full-rate video belongs on a
host-owned producer/surface path, where the host keeps the buffer and the Spore
only names the surface.

Screencast is a host-owned producer. The host shows its own picker, owns the
capture and encoding, and returns a surface alias. A Spore can start and stop
the session; it cannot enumerate displays, choose a native window handle or read
the frame buffer. This is also why `soil:screencast` is not a per-frame API:
screen content is the highest-risk capture source, and the host must keep the
pixels.

## Limits and privacy

All three contracts are polled, bounded and one-session-per-Spore:

- a visible host capture indicator is mandatory;
- no background capture: closing or dropping the resource stops capture;
- the host chooses the device or surface; the guest supplies only a user-facing
  `purpose` string for the consent surface;
- microphone: 16 kHz to 48 kHz, mono or stereo, at most 8192 frames per read;
- camera: at most one frame in flight and at most 8 MiB per frame;
- screencast: host policy sets resolution, frame rate and bitrate; the guest
  gets no encoder or display controls.

A denial is a named error (`denied`, `cancelled`, `not-ready`, `over-budget`,
`closed`, and so on), never a placeholder success.

## Rooms

The Rooms Spore does **not** request `soil:mic`, `soil:camera` or
`soil:screencast`. Its snapshot carries room state, not devices: `self.mic`, the
member `speaking`/`mic` fields, and `room.screens[]` surface aliases. The host
room adapter owns capture:

```text
Rooms Spore --share-surface / mic intent--> host room adapter
                                              |
                    soil:mic  soil:camera  soil:screencast
                                              |
                         speech/program relay or surface alias
                                              |
Rooms Spore <---- state + opaque aliases -----+
```

This keeps three things out of Lua: the device permission prompt, the encoded
media buffer, and the room transport. A Spore that has both `soil:screencast`
and `soil:surface-import` may place the returned alias itself; the Rooms Spore
only needs the host-projected alias in `room.screens[]`.

`camera` needs one extra rule: a room camera is a host-owned producer, not a
`soil:camera` frame loop in the Spore. The still/low-rate contract is for camera
apps and previews; the room adapter publishes a bounded surface.

## Host backends

| Platform | Microphone | Camera | Screencast |
|---|---|---|---|
| macOS | AVFoundation | AVFoundation | ScreenCaptureKit |
| Linux | PipeWire/ALSA | V4L2/PipeWire | PipeWire portal |
| Android / Fire TV | AudioRecord | Camera2 | MediaProjection |
| Browser | `getUserMedia` + AudioWorklet | `getUserMedia` | `getDisplayMedia` |

These are host adapters, not guest ABI. The first backend is
[`crates/soil-capture`](../crates/soil-capture): `CpalMicSource` uses the default
input device and `FfmpegCameraSource` asks AVFoundation for low-rate RGBA frames
through ffmpeg. The camera adapter is a deliberately small first path; a native
AVFoundation/PipeWire backend can replace it behind the same trait.

The WIT host linker is still pending. The contracts and `soil-abi` checks are in
place so the adapters can land without changing the guest surface.

## Test the adapters

```sh
cargo test -p soil-capture
cargo run -p soil-capture --bin soil-capture-probe -- mic --seconds 3
cargo run -p soil-capture --bin soil-capture-probe -- camera --device 0 \
  --seconds 2 --out /tmp/frame.ppm
```

The camera probe uses ffmpeg's `avfoundation` input on macOS; pass the device
index from `ffmpeg -f avfoundation -list_devices true -i ''`. The ffmpeg adapter
opens an AVFoundation device exclusively: on the test Mac, microphone capture
and camera capture against the same USB webcam did not run concurrently. A call
that needs both should use separate devices or a single native AVFoundation
session.

## Next slices

1. Link the mic/camera host adapters into the component linker.
2. Microphone capture and the `speech` relay for Rooms phase 2.
3. `soil:screencast` capture and the surface/relay path for Rooms phase 5.
4. A camera surface producer for the room camera tile; keep `soil:camera` for
   stills and low-rate previews.
