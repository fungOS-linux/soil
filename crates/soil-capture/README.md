# soil-capture

Host-side adapters behind `soil:mic` and `soil:camera`.

- `CpalMicSource` opens the default input device and returns interleaved f32
  chunks. The cpal stream stays on a dedicated thread; the guest-facing session
  is polled.
- `FfmpegCameraSource` asks AVFoundation for low-rate RGBA frames through
  ffmpeg. It is a first backend, not a per-frame guest copy path; a native
  AVFoundation or PipeWire adapter can replace it behind `CameraSource`.
- `FixtureMicSource` / `FixtureCameraSource` are deterministic sources for
  tests.

These are host backends. The component linker that exposes them as
`soil:mic/capture` and `soil:camera/capture` is still pending.

## Run

```sh
cargo test -p soil-capture
cargo run -p soil-capture --bin soil-capture-probe -- mic --seconds 3
cargo run -p soil-capture --bin soil-capture-probe -- camera --device 0 \
  --seconds 2 --out /tmp/frame.ppm
```

The camera probe requires ffmpeg with AVFoundation support on macOS. List
devices with:

```sh
ffmpeg -f avfoundation -list_devices true -i ''
```

The ffmpeg backend opens an AVFoundation device exclusively. On this Mac,
running microphone capture and camera capture against the same USB webcam at
the same time produced no camera frames until the input device was released.
Use separate devices, or replace this backend with one AVFoundation session for
mic + camera before relying on simultaneous capture in a call.

## Limits

- microphone: one session per Spore, 16 kHz..48 kHz, mono/stereo, at most 8192
  frames per read;
- camera: one session per Spore, at most one frame in flight, at most 8 MiB per
  frame;
- capture is visible and foreground-only; closing or dropping a session stops
  the backend.
