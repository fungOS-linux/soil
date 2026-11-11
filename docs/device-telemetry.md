# Device telemetry

Status: **design, 2026-11-10.** This is the model for BLE wearable and edge
node data. It is not a WIT package yet. Phase 1 implements the first
`soil-ble-sim` and the PineTime Spore against it; the model is deliberately
written before the simulator so the simulator has something honest to fake.

## Boundaries

- **Telemetry is a record, not an action.** DCP actions (`device.sync`,
  `device.find`, `notify`, `capture.start/stop`) are separate from signal
  records. An action has a receipt; a signal has provenance.
- **Durable history lives in `soil:records`.** Live state and low-rate events
  use `soil:unibus`. `soil:unibus` is a live tail, not a history store; a
  cursor that predates retention is `stale` by contract.
- **Raw audio is never a bus body.** A Spore publishes an `audio.segment.v1`
  record with a CID, not PCM or Opus bytes. The blob belongs in
  `soil:storage`.
- **The host owns the radio, clock, storage, and grants.** A Spore owns the
device protocol and the meaning of a signal. A Spore never receives a BLE
address, device handle, credential, or path it can forge.
- **Live device reads and durable telemetry are different.**
  `soil:device.battery` is an invocation-scoped, fresh/unknown/stale/denied
  read for a guest that is presenting a value now. `device.state.v1` is the
  durable record used for history, fusion, and sync. Neither is a substitute
  for the other; a stale live read never becomes a zero, and a historical
  record never pretends to be current.

## Identity

Five identities are separate. Collapsing them is how a ring worn by a guest
becomes the owner's health record.

| Identity | Meaning | Source |
|---|---|---|
| `principal_id` | Authenticated person or account | Mycelium/OIDC |
| `wearer_id` | Who is wearing the device now | Explicit, optional, may be unknown |
| `device_id` | Stable hardware identity | Serial or DFU UUID, never the BLE address |
| `bridge_id` | Installation that owns the radio | The `soil:ble` host |
| `spore_id` | Signed package/manifest identity | Spore manifest |

`device_id` is `<kind>:<serial>` or `<kind>:<hash>` where the serial is read
from the device's DIS or provisioning record. If the hardware exposes no
serial, the host assigns a stable ID at enrollment and records the pairing
fingerprint. A BLE address is transport metadata, not identity.

### Enrollment

Collection `device`, schema `fungos.device/enrollment.v1`:

```json
{
  "kind": "wearable.watch",
  "model": "PineTime",
  "hardware_revision": "nRF52832",
  "firmware": "InfiniTime 1.16.0",
  "serial": "...",
  "capabilities": ["body.hr", "body.motion", "notify", "media"],
  "owner": "principal:...",
  "bridge": "installation:...",
  "trust": "paired",
  "enrolled_at_ms": 0,
  "revoked_at_ms": null
}
```

### Device state

Collection `device`, schema `fungos.device/state.v1`:

```json
{
  "state": "leased",
  "battery_percent": 72,
  "charging": false,
  "link": "connected",
  "lease": { "host": "...", "expires_at_ms": 0 },
  "last_seen_ms": 0,
  "rssi_dbm": -61,
  "firmware": "..."
}
```

State machine:

```text
unknown -> present -> connected -> leased -> stale
                                         \-> revoked
```

One lease per device, with a TTL. A second host consumes events rather than
connecting. This mirrors the existing `soil:ble` radio lease: a crashed host
must lose the device within a minute, not hold it forever.

## Time

Every signal carries:

| Field | Meaning |
|---|---|
| `observed_at_device_ms` | Device clock at observation |
| `received_at_host_ms` | Host receive time, authoritative for ordering |
| `age_ms` | Host-computed age at read |
| `clock_uncertainty_ms` | Bound on device clock error |
| `sync_epoch` | Which clock synchronization the device time belongs to |

Host receive time orders signals. Device time is only for intervals and
display. Never compare raw device clocks across devices; two watches can
differ by seconds and a ring can drift for days.

Freshness is `fresh | stale | expired`. Stale is **unknown**, not zero; a
missing sample never becomes a normal reading.

## Signal envelope

```json
{
  "signal_id": "content-key",
  "kind": "body.hr.v1",
  "schema": "fungos.signal/hr.v1",
  "source_device": "wearable.watch:...",
  "source_bridge": "installation:...",
  "source_spore": "dev.fungos.pinetime@0.1.0",
  "wearer_id": "principal:...",
  "observed_at": 0,
  "received_at": 0,
  "age_ms": 0,
  "quality": { "state": "good", "score": 0.92, "reason": null },
  "provenance": "measured",
  "model_version": null,
  "privacy": "health",
  "retention": "summary_90d",
  "value": { "bpm": 61, "contact": "good" },
  "blob_ref": null
}
```

- `provenance` is `measured | derived | inferred | imported`. Sleep stages are
  `derived`; a transcript is `derived` from audio; a weather snapshot is
  `imported`.
- `quality.state` is `good | degraded | unknown | denied | unsupported`.
  A denied sensor is not zero and not `null`; it is a named state.
- `privacy` is `device | health | ambient_audio | location | presence`.
- `retention` is `ephemeral | raw_72h | summary_90d | derived_1y`.
- `value` is typed JSON for the signal; `blob_ref` is a CID for audio or an
  image. Both are bounded by the host.

## Signal catalog

| Kind | Producer | Rate | Durable store | Live bus |
|---|---|---|---|---|
| `device.state.v1` | Bridge | On change, 1/min | `device` | latest-value |
| `body.hr.v1` | Ring/watch | 1 Hz or duty-cycled | `signal` summary | threshold only |
| `body.hrv.v1` | Ring | Per sleep/session | `signal` | no |
| `body.spo2.v1` | Ring | 1–5 min | `signal` | no |
| `body.temp.v1` | Ring | 1–5 min | `signal` | no |
| `body.sleep.v1` | Host model | Per night | `sleep` | on completion |
| `body.motion.v1` | Ring/watch/OMI | 1–25 Hz local, aggregates | `signal` | no raw |
| `body.steps.v1` | Ring/watch | 1/min | `signal` | no |
| `audio.segment.v1` | OMI/S22 | Per VAD segment | `audio` + CID | metadata only |
| `presence.state.v1` | Presence | 1 Hz | `presence` | latest-value |
| `location.fix.v1` | S22 | On change | `location` | no |
| `agent.event.v1` | Operator/JARVIS | On event | `agent` | append-only |

Raw motion at 25 Hz is a local buffer format, not a durable signal. Store
activity counts, downsampled axes, or a bounded window.

`body.sleep.v1` is a completed-night host model. Its value carries the stage
timeline, not one instantaneous stage:

```json
{
  "start": 1794352740000,
  "end": 1794381540000,
  "stages": [
    {"stage": "awake", "start": 1794352740000, "end": 1794353940000},
    {"stage": "light", "start": 1794353940000, "end": 1794359340000},
    {"stage": "deep",  "start": 1794359340000, "end": 1794364140000},
    {"stage": "rem",   "start": 1794364140000, "end": 1794367140000}
  ]
}
```

`observed_at` is the session end. The consumer selects one source for the whole
night; stages are never averaged or stitched across devices. Ring physiology is
primary, watch is fallback, and the greater stage coverage breaks a tie. A
completed session remains usable through `sleep_max_age_ms` (12 h by default)
and is marked stale rather than silently treated as current after that.

## Fusion and precedence

Each signal has a written precedence policy. It is not inferred at read time
by whoever asks first.

| Signal | Primary | Fallback | Max age | Conflict rule |
|---|---|---|---|---|
| Heart rate | Ring | Watch | 5 min | No average; highest contact quality |
| Sleep | Ring physiology + watch motion | Either device | 12 h | No average stages; coverage-weighted |
| Steps | One counter per wearer | Other device | 1 day | Never sum devices |
| SpO2/temp | Ring | none | 15 min | Unknown, not zero |
| Presence | Camera/CSI presence | BLE proximity | 30 s | RSSI is evidence, not identity |
| Audio | OMI ambient | S22/AZ09 voice | session | One active capture session |
| Location | S22 GPS/Wi-Fi | Phone | 5 min | Accuracy and source recorded |

Two rings worn by the same person are **not** a redundancy win. They duplicate
HR, sleep, and steps and may disagree. Record `wearer_id`, choose one primary,
and keep the other as a spare or assign it to a different wearer.

## Transport

Kinds are versioned in the suffix. Bodies are JSON objects; the host enforces
the 64 KiB ceiling and never reads the fields.

- `device.state.v1` — latest-value, TTL, no history.
- `presence.state.v1` — latest-value, TTL, no history.
- `signal.sample.v1` — only low-rate signals; not raw audio or motion.
- `signal.summary.v1` — batched aggregates; the normal path.
- `audio.segment.v1` — metadata + CID; no audio bytes.
- `agent.event.v1` — Operator/JARVIS events; append-only.

`soil:unibus` is a live tail. Durable history and sync use
`soil:records.changes`, not a bus cursor. A Spore that needs replay reads the
record collection; a Spore that needs freshness reads the bus.

Idempotency is content-keyed: `(device_id, kind, observed_at, sequence)` for
samples; `(source, delivery_id)` for imported events. `soil:unibus` has no
idempotency key by contract.

## Storage

| Collection | Content | ID |
|---|---|---|
| `device` | Enrollment and current state | `<device_id>` |
| `signal` | Samples and summaries | `<kind>:<device>:<observed_at>:<seq>` |
| `sleep` | Nightly derived stages | `<wearer>:<night>` |
| `audio` | Segment metadata | `<cid>` |
| `presence` | Zone snapshots and transitions | `<zone>:<observed_at>` |
| `location` | Fixes | `<device>:<observed_at>` |

Audio and images are content-addressed blobs in `soil:storage`; records hold
the CID, codec, duration, VAD result, transcript reference, and privacy class.

Retention is per class: raw samples 72 h, summaries 90 d, derived sleep/health
1 y, agent events 90 d. Export and delete are per wearer and per device.

## Battery and scheduling

- **Event-driven BLE.** Subscribe to GATT notifications; do not poll. Scan
  windows are 1–5 s every 30–60 s for presence, not continuous.
- **Batch, do not stream.** Ring/watch/OMI buffer locally; sync on connect, on
  charge, every 5–15 min, or on explicit user action.
- **Duty-cycle HR.** 1 Hz only during sleep or a workout; 1 min every 5 min
  otherwise. Continuous HR is the watch's dominant drain.
- **Watch flash.** 4 MB LittleFS; HR at 1 Hz is about 173 KB/day. Raw motion
  at 25 Hz is about 13 MB/day, so store aggregates or downsample.
- **OMI.** 512 MB NAND buffers audio; VAD and transcript on the host; sync on
  Wi-Fi/phone. The 10–14 h vendor figure is continuous capture, not 24/7.
- **S22.** Suspend and wake-source policy gate always-on. Until then, sync in
  windows and treat the phone as a duty-cycled bridge.

## Privacy

- Health data is local-first and per wearer. Presence gating applies before
  disclosure; a guest wearing a ring is not the owner's data.
- Ambient audio requires a visible capture indicator, explicit consent, a
  retention class, and redaction before any model or cloud path.
- Location is coarse by default; precise fixes require a grant.
- Revoking a device or bridge deletes local signal data according to its
  retention class and stops further sync.

## Actions, not telemetry

DCP actions are separate from signals and carry receipts:

| Action | Effect |
|---|---|
| `device.sync` | Flush a device's local buffer |
| `device.find` | Buzz/LED a device |
| `notify` | Send a notification to a surface |
| `capture.start` / `capture.stop` | Arm or disarm a capture session |
| `media.play` / `media.pause` | Control the active media route |

An action never writes a raw signal. A signal never implies an action. The
mapping goes through `fungos:actions` with exact grants, approval class, and
idempotency.

## Simulation

Phase 1’s simulator must be able to lie deterministically:

- Virtual GATT services for PineTime, Ring 2, OMI, and S22.
- Clock drift, battery drain, packet loss, disconnects, stale data, denied
  sensors, and a second host contending for the lease.
- Replay of recorded sessions; golden records for every signal kind.
- Property tests: no summed steps, no cross-wearer attribution, stale is not
  zero, no raw audio on the bus, one lease per device.

## Open questions

1. What does the COLMI Ring 2 protocol actually expose for sleep stages,
   SpO2, and tap/gesture events?
2. Which devices expose a hardware serial usable as `device_id`?
3. What is the sleep fusion model and its confidence calibration?
4. Does OMI firmware expose IMU/button events over BLE, or only audio?
5. What is the S22 suspend/wake-source sequence and which devices can wake it?
6. What is the Mycelium edge profile for slow-poll peers?

## Non-goals

- A raw per-sample firehose on the bus.
- Cloud-first health or ambient audio.
- BLE address as identity.
- Cross-wearer attribution.
- Telemetry inside DCP catalogs.
- A Spore owning the radio or a device handle.
