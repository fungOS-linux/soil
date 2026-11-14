# soil-ble-sim

A deterministic, GATT-shaped stand-in for the radio behind `soil:ble`. It is not
a Bluetooth emulator: it exists so the host and the first device Spores can be
developed and tested without hardware, and so a bad day can be replayed exactly.

```sh
SOIL_BLE_SIM=1 cargo run -p soil-ble-host -- --scan 100
SOIL_BLE_SIM=1 cargo run -p soil-ble-host -- module.wasm watch --hold-ms 1000
```

`SOIL_BLE_SIM_CONTENDER=1` also makes a second host claim the first virtual
device before the guest starts, which exercises the lease refusal path.

## Virtual devices

- `sim-pinetime` — InfiniTime services: DIS, HRS, BAS, ANS, IAS, CTS, motion
  (`00030000-78fc-48fe-8e23-433b3a1942d0`), and music
  (`00000000-78fc-48fe-8e23-433b3a1942d0`). HR is the two-byte InfiniTime form,
  steps are uint32, motion is three int16 axes, and the ANS/music characteristics
  are writable through the same `soil:ble` verbs the real host exposes.
- `sim-ring` — HRS/BAS plus the QRing vendor characteristic, still synthetic
  until the vendor protocol is mapped.
- `sim-omi` — battery plus the OMI audio data/codec characteristics.

## Controls and faults

Every operation advances or observes a virtual `tick`; nothing reads the wall
clock. `SimBackend::battery()` and `SimBackend::last_write()` expose state for
assertions.

`SimDevice::faults` injects:

| Fault | Effect |
|---|---|
| `drop_every` | drop every Nth tick's notifications |
| `disconnect_at` | emit one `Disconnected` event and clear the link |
| `stale_after` | reads after this tick fail as stale |
| `denied` | listed characteristic UUIDs fail as denied |

Battery drains by `drain_per_tick` and clamps at zero. The built-ins use
InfiniTime's real UUIDs and payload widths, so a protocol regression shows up in
the golden replay rather than only on a watch.

## Tests

The crate covers scan/connect/read/write/subscribe, packet loss, disconnect,
stale and denied reads, battery drain, OMI framing, real InfiniTime service
presence, a byte-for-byte golden replay, and the policy properties:

- steps stay per device and are never summed;
- an owner cannot see a guest's signals;
- stale/expired values are unknown, never zero;
- raw audio is refused by the bus and only a `blob_ref` is publishable.

Run them with:

```sh
cargo test -p soil-ble-sim
```

## What it does not model

Advertising timing, PHY/DLE, pairing and encryption, L2CAP fragmentation, real
RSSI variation, or vendor protocol semantics. A passing simulator test is
evidence about the host/Spore boundary, not a certification of a radio or a
device.
