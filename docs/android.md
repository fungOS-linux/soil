# Soil on Android

Status: **first device proof, 2026-11-01.** wasmtime runs inside the Canvas
Android app and renders the actual Canvas owner through `soil:canvas`.

## Proof

- Device: FireTV Omni `AFTTIFF43`, ARMv7, API 28, `10.0.0.91:5555`.
- App: `dev.fpl.canvas.tv` `0.1.64-soil`.
- Surface: workspace `soil`, content `{"kind":"soil"}`.
- Rendered: Canvas Work Inbox (actual `canvas-workspace-ui` owner, DEMO DATA).
- Input: D-pad moved the selection, OK opened the detail pane, Back returned.

No Soil host, video stream or second process is involved. The guest, wasmtime,
the Canvas owner and the frame handoff all run in the Android app.

## Architecture

```
TvActivity
  └─ SoilSurface (Java ImageView)
       └─ single-thread executor
            └─ libsoil_android.so (JNI)
                 └─ soil-runtime::GuestScene
                      └─ embedded CANVAS_GUEST (core wasm)
                           └─ soil:canvas present/input
                                └─ OwnerSurface (Canvas Work Inbox)
                                     └─ ARGB int[] -> Bitmap -> ImageView
```

- `crates/soil-android` is the `cdylib`; its lib name is `soil_android`.
- Java loads it with `System.loadLibrary("soil_android")`.
- One executor serializes `nativeCreate`, `nativePresent`, `nativeInput` and
  `nativeDestroy`; the wasmtime store is never used concurrently.
- The guest receives only the granted Canvas surface. It never sees a window,
  Bitmap, framebuffer, display id or raw keycode.
- Frames are 480×640 RGBA from the owner, converted to ARGB ints for Android.
  The view scales/letterboxes them to the TV.

## Build the JNI library

```sh
NDK=$HOME/Library/Android/sdk/ndk/26.3.11579264
export CC_armv7_linux_androideabi="$NDK/toolchains/llvm/prebuilt/darwin-x86_64/bin/armv7a-linux-androideabi24-clang"
export AR_armv7_linux_androideabi="$NDK/toolchains/llvm/prebuilt/darwin-x86_64/bin/llvm-ar"
export CARGO_TARGET_ARMV7_LINUX_ANDROIDEABI_LINKER="$NDK/toolchains/llvm/prebuilt/darwin-x86_64/bin/armv7a-linux-androideabi24-clang"

cargo build -p soil-android --target armv7-linux-androideabi --release
```

Result: `target/armv7-linux-androideabi/release/libsoil_android.so`.

The Canvas app also needs `libcanvas_android.so` from its normal ARMv7 build.
Package both under `lib/armeabi-v7a/` in the APK.

## Controls

The Java view maps the FireTV remote to the runtime's normalized keys:

| Remote | Soil key |
|---|---|
| D-pad up/down/left/right | 1/2/3/4 |
| OK / Enter | 5 (Select) |
| Back | 6 |
| Home | 7 |

Arrows repeat while held; Select/Back/Home are single-shot. Unknown keys stay
host-side.

## Limits of this proof

- The guest is the embedded `CANVAS_GUEST`; there is no Spore loading,
  manifest verification, signing or revocation yet.
- The runtime still contains S22-specific adapters via path dependencies. The
  Arduino app proof works because they cross-compile; a later split should make
  the core runtime independent.
- No `soil:view` WebSurface integration in this proof.
- `soil:canvas` generation/lifecycle is not enforced by this Java bridge yet;
  the runtime's simpler core-Wasm ABI is used.
- The frame loop is 4 fps. That is deliberate for a first proof, not a
  performance claim.
