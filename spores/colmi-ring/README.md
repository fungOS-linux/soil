# COLMI ring Spore

The COLMI ring is the first real BLE workload for Soil. It is split by the same
rule as every other capability: **transport in Soil, protocol in the Spore**.

| Piece | Path | Owns |
|---|---|---|
| Contract | [`wit/soil-ble.wit`](../../wit/soil-ble.wit) | 8 bounded BLE verbs, typed errors |
| Host | [`crates/soil-ble-host`](../../crates/soil-ble-host) | wasmtime 50, btleplug/BlueZ/CoreBluetooth, radio lease |
| Spore | [`spores/colmi-ring`](.) | 16-byte framing, bind, battery, command semantics |

This code moved here from `FuturePresentLabs/reversal` (`colmi/host`) so the
capability and its only host have one home. The device protocol remains a Spore,
not part of the ABI.

## Targets

| Target | BLE backend | Shape |
|---|---|---|
| macOS | CoreBluetooth via `btleplug` | `soil-ble-host` binary |
| desktop Linux | BlueZ via D-Bus inside `btleplug` | `soil-ble-host` binary |
| CAT S22 / Fire TV (Android) | Android Bluetooth GATT via JNI | Rust core as `cdylib`, thin Kotlin Activity |

## Backends and D-Bus

The Spore never sees a backend. `soil:ble` is the same WIT on every target.

- **macOS:** `btleplug` uses CoreBluetooth. There is no D-Bus dependency or
  runtime requirement. `libdbus-sys` is gated to Linux only; it is not built or
  linked on macOS.
- **Linux:** BlueZ exposes BLE over D-Bus, so `btleplug` needs a running
  `dbus-daemon` and BlueZ. The vendored `libdbus-sys` on Linux is only a static
  packaging choice for the S22/musl host; it does not add a second IPC layer.
- **Android / Fire TV / S22:** use the Android Bluetooth stack through JNI, not
  BlueZ and not D-Bus. That backend is still pending.
- **If fungOS has no D-Bus:** BlueZ-over-D-Bus cannot work. The transport-
  agnostic options are a direct HCI backend on Linux (privileged, device-
  specific) or the Android JNI backend. Neither changes the Spore or the WIT.

On macOS the host binary can be used without a guest for a radio smoke test:

```sh
cargo run -p soil-ble-host -- --scan 4000
```

This prints CoreBluetooth peripheral UUIDs, RSSI and local names.

The radio is always local, so swapping backends never changes a Spore.

```
┌───────────────────────────────────────────────┐
│ ring-spore.wasm     imports soil:ble/ble only │
│   framing, commands, state machines, policy   │
└───────────────────────┬───────────────────────┘
                        │ WIT (wit/soil-ble.wit)
┌───────────────────────▼───────────────────────┐
│ soil-ble-host                                 │
│   wasmtime + capability linker                │
│   ├── ble backend:  btleplug | android-jni    │
│   └── session store: creds, identity, cursor  │
└───────────────────────┬───────────────────────┘
                        │ Unibus kinds (mesh)
┌───────────────────────▼───────────────────────┐
│ other hosts / consumers                       │
│   ring.dis (schema device.v1)                 │
│   ring.health.sample (schema sample.v1)       │
└───────────────────────────────────────────────┘
```

## Why the Spore owns the protocol

The ring's wire format (16-byte `[cmd][payload][checksum]`, `0x80` error bit,
`0xBC` bulk path, `de5bf728` DFU) changes far more often than the radio API.
Putting it in WASM means a protocol fix ships to both hosts as one `.wasm`
file, and a bad Spore cannot widen the host's capability surface — it can only
call the eight BLE verbs it was given.

## Radio ownership is a lease, not a cluster

- **Radio ownership.** At most one host drives the ring at a time. A host
  claims a `Lease { host_id, ring_id, claimed_at, expires_at }` before
  connecting; a second host that hears the ring refuses to connect and instead
  consumes events. Leases are advisory and short (default 60 s, renewed while
  connected) so a crashed host loses the ring within a minute.
- **Shared credentials.** `SessionCreds { token, uid, ring_id, address,
  hardware_version, firmware_version }` is written once and read by every
  host. This is what makes pairing unnecessary.
- **Cursor state.** Bulk reads are positioned by
  `Cursor { ring_id, stream, since }` so another host resumes instead of
  re-downloading history.
- **Events.** Hosts publish `ring.*` as Unibus kinds; consumers do not care
  which host saw the ring.

The lease in this tree is still **local and unsigned** (file-backed JSON). The
signed, fleet-identity lease from
[`mycelium/docs/soil-capabilities.md`](https://github.com/fungOS-linux/mycelium/blob/master/docs/soil-capabilities.md)
is not wired yet.

Deliberately **not** in v1: multi-host simultaneous BLE links (the S22 and
desktop would fight over the GATT link) and any cloud dependency.

## Status

| Piece | State |
|---|---|
| `soil:ble@1.0.0` WIT | canonical in `wit/soil-ble.wit` |
| wasmtime host + linker | working (sync host calls, one runtime) |
| desktop BLE backend | working: scan / connect / read / write / notify |
| session store (creds, lease, cursors) | file-backed, trait-isolated |
| notification UUID preservation | working; the Spore sees `notification(char-uuid, data)` |
| disconnected event | emitted when the notification pump ends |
| Android backend | **not yet** — needs a JNI bridge |
| Unibus publishing | **not yet** — events are local |
| `ring-id` from DIS | **not yet** — `ring-id` is `none` |
| signed lease | **not yet** — local JSON |

## Android (CAT S22) path

There is no usable userspace BlueZ on Android and `btleplug` does not target it,
so the Android build is:

1. `soil-ble-host` compiled as a `cdylib` for `aarch64-linux-android`
   (`cargo ndk -t arm64-v8a build`), exporting a small C ABI:
   `soil_ble_host_new`, `soil_ble_host_scan`, `soil_ble_host_connect`,
   `soil_ble_host_run`.
2. A Kotlin `BluetoothGattCallback` implementing the same `BleBackend` trait
   through JNI: scan results, characteristic reads/writes, notification
   dispatch. The JNI layer must copy buffers immediately — never hold a JNI
   reference across a suspend point.
3. The `soil:ble`-importing Spore compiled once, shipped inside the APK and
   loaded with `Component::from_file` from the app's asset dir.

Permissions on S22 that will bite: `BLUETOOTH_SCAN` with `neverForLocation`,
`BLUETOOTH_CONNECT` at runtime, and no background scanning without a foreground
service. Those are Kotlin-side concerns; the Spore never sees them.

## Build and run

```bash
# Spore (protocol logic)
rustup target add wasm32-wasip2
cargo build --manifest-path spores/colmi-ring/Cargo.toml \
  --target wasm32-wasip2 --release

# desktop host
cargo run -p soil-ble-host -- \
  spores/colmi-ring/target/wasm32-wasip2/release/colmi_ring_spore.wasm
```

The Spore accepts optional arguments: `[match] [scan-ms] [hold-ms]`.
`match` is a handle, name substring or ring id. With no match it looks for a
name containing `colmi` or `ring`, binds the vendor channel, asks for battery,
and drains notifications.
