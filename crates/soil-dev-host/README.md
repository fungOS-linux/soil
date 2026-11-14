# soil-dev-host

A desktop composition for running one Spore end to end:

```text
soil:ble + soil:http + soil:records + soil:unibus + soil:time
```

The BLE path is the same one `soil-ble-host` uses. By default it runs
`soil-ble-sim` with 50 ms pacing; `--real` selects CoreBluetooth on macOS or
BlueZ on Linux. Records use `soil-host`'s namespaced adapter. Unibus is
an in-memory `MemoryBus`, so published frames are visible for the life of the
process but do not persist.

`soil:http` is `soil-host`'s own implementation (`HttpHost`), linked with an
empty allowlist. A Spore that needs the network is given the origins it needs
one at a time with `--allow-origin`; everything else is refused with `denied`,
so a missing origin is a visible failure rather than a silent fallback.

## Usage

```sh
cargo build -p soil-dev-host

# PineTime watch -> signal/device.state records
target/debug/soil-dev-host \
  --records-dir .soil-dev/records \
  path/to/pinetime/module.wasm \
  watch --wearer owner --hold-ms 1000

# JARVIS development data, no hardware
target/debug/soil-dev-host --records-dir .soil-dev/records \
  path/to/jarvis-telemetry/module.wasm seed
target/debug/soil-dev-host --records-dir .soil-dev/records \
  path/to/jarvis-telemetry/module.wasm once --wearer owner

# Vitals summary + latest bus frame
target/debug/soil-dev-host --records-dir .soil-dev/records \
  path/to/vitals/module.wasm seed
target/debug/soil-dev-host --records-dir .soil-dev/records \
  path/to/vitals/module.wasm once --wearer owner

# Weather -> weather.current record, then the watch reads it back
target/debug/soil-dev-host --records-dir .soil-dev/records \
  --allow-origin https://api.open-meteo.com \
  --allow-origin https://geocoding-api.open-meteo.com \
  path/to/weather/module.wasm now --place "Seattle"
target/debug/soil-dev-host --records-dir .soil-dev/records \
  path/to/pinetime/module.wasm weather --from-record
```

`once` prints the fused snapshot and then each Unibus frame the guest
published, for example:

```text
bus presence.state.v1 {"state":"present","wearer":"owner",...}
bus attention.state.v1 {"state":"focused","wearer":"owner",...}
```

## Records and leases

- The dev host grants six namespace names to the guest: `signal`,
  `device.state`, `vitals.summary`, `vitals.latest`, and `vitals.view` in the
  `dev/personal/telemetry` book, plus `weather.current` in
  `dev/personal/weather`, so the weather Spore writes and the PineTime Spore
  reads the same record.
- The radio lease uses the same `SessionStore` as `soil-ble-host`; the default
  session is `.soil-dev/soil-ble-session.json`.
- `--records-dir`, `--session`, and `--host-id` override the defaults;
  `SOIL_RECORDS_ROOT`, `SOIL_BLE_SESSION`, and `SOIL_HOST_ID` are the env
  equivalents.
- `--allow-origin URL` is repeatable and grants exactly that origin; with none,
  every `soil:http` request fails with `denied` and the host says so at
  startup.

## Limits

This is a development composition, not the production daemon:

- one guest per process;
- the Unibus is in-memory and dies with the process;
- `soil:time` uses a dev-only epoch persisted as
  `<records-dir>/time-origin-ms`, so consecutive runs share one forward-moving
  timeline. Production `soil:time` remains process-relative.
- the simulator's 50 ms pacing is a convenience, not a real sensor rate.

The next step beyond this host is a long-lived device supervisor that owns the
records store and bus across guest restarts.
