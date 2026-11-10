# Device profiles

Status: **design inventory, 2026-11-10.** These are the profiles the Phase 1
simulator and the first Spores implement. They are not a WIT package. A
profile describes what a device exposes and how it is allowed to be used; the
contract remains `soil:ble`, `soil:time`, `soil:records`, and `soil:unibus`.

Each profile records:

- **identity**: how a stable `device_id` is obtained, and what is not identity;
- **transport**: BLE GATT, Bluetooth Classic, or a host-local bus;
- **services**: UUIDs, characteristics, direction, payload, and rate;
- **signals**: the `device-telemetry` kinds the device can produce;
- **commands**: writes and notifications the host may send;
- **battery**: capacity, measured life, charge time, and the model used by the
  simulator;
- **constraints**: connection count, MTU/DLE/PHY, background behavior;
- **today**: what is actually readable or writable now, not what the hardware
  could do in principle.

## PineTime / InfiniTime

| Field | Value |
|---|---|
| Kind | `wearable.watch` |
| SoC | nRF52832, 512 KB flash, 64 KB RAM, 4 MB external LittleFS |
| Firmware | InfiniTime 1.16.x, FreeRTOS + NimBLE + LVGL 7.11 |
| Transport | BLE 5.0 GATT; 1 Mbps; DLE off; 2M PHY off |
| Identity | DIS serial when present; otherwise host enrollment fingerprint |
| Battery | 170–180 mAh; 3–7 days with notifications; 1–2 days continuous HR; 1.5–2 h charge |
| Constraints | One BLE connection; ~23-byte ATT payloads; no NUS; no runtime apps or faces |

### Services

| Service | UUID | Direction | Notes |
|---|---|---|---|
| Device Information | `0x180A` | read | Model, firmware, hardware, serial |
| Heart Rate | `0x180D` (`0x2A37`) | notify | HR and RR intervals where available |
| Battery | `0x180F` (`0x2A19`) | read/notify | Percent |
| Current Time | `0x1805` | both | Watch is both CTS server and client |
| Alert Notification | `0x1811` (`0x2A46`) | both | Notifications and categories |
| Immediate Alert | `0x1802` (`0x2A06`) | write | Buzz now |
| Music | `00000000-78fc-…` | both | Track metadata and transport controls |
| Navigation | `00010000-78fc-…` | write | Turn-by-turn |
| Call/notification event | `00020001-78fc-…` | notify | Call state |
| Motion | `00030000-78fc-…` | notify | Steps and motion events |
| Simple Weather | `00050000-78fc-…` | write | Current/forecast |
| DFU | `0x1530` | both | Firmware update; 20-byte chunks |
| File System | `0xFEBB` | both | LittleFS read/write for resources and logs |

### Signals and commands

- `body.hr.v1` at 1 Hz when subscribed; `body.motion.v1` at the firmware rate
  or as step/activity aggregates; `device.state.v1` battery/link.
- Commands: notify, IAS buzz, media transport, weather, navigation, time set,
  DFU, FS read/write.
- **Buffer, do not stream:** 4 MB LittleFS; HR at 1 Hz is about 173 KB/day.
  Raw motion at 25 Hz is about 13 MB/day; store aggregates or downsample.
- **Today:** InfiniTime companion apps can read HR/battery and send
  notifications; no custom face or app can be loaded at runtime. A JARVIS
  face or a custom service is a firmware change.

## COLMI Ring 2

| Field | Value |
|---|---|
| Kind | `wearable.ring` |
| Hardware | Realtek-family BLE SoC, ~3.5 g, 7 mm band |
| Transport | BLE 5.2 |
| Identity | No confirmed hardware serial; host enrollment fingerprint; BLE address is not identity |
| Battery | ~5 days typical, 15 days standby, 1.5 h charge |
| Constraints | One connection; proprietary QRing protocol; two rings must be treated as separate devices/wearers |

### Signals

- `body.hr.v1`, `body.hrv.v1`, `body.spo2.v1`, `body.temp.v1`, `body.sleep.v1`,
  `body.steps.v1`; stress and cycle reminders are app-level derived fields.
- Tap/gesture controls exist on supported variants. Use the ring's
  accelerometer interrupt/tap mode; do not stream raw acceleration.
- **Today:** the QRing app is the reliable source for sleep stages, SpO2, and
  temperature. The direct BLE protocol is not mapped in this repo; the existing
  COLMI research covers the R02/R11 families and the experimental DFU client.
- **Phase 1 unknown:** whether the Ring 2 exposes HR/SpO2/sleep to a
  third-party central, or only to QRing. The profile must stay honest until
  that is answered.

## OMI CV1

| Field | Value |
|---|---|
| Kind | `wearable.pendant` |
| SoC | nRF5340 dual-core; nRF7002 Wi-Fi 6 (disabled in current firmware) |
| Storage | 512 MB soldered SD NAND, raw audio ring |
| Audio | Dual TDK T5838 PDM microphones |
| Sensors | LSM6DS3TR-C 6-axis IMU; button; RGB LEDs; vibration motor |
| Transport | BLE 5.0; Wi-Fi not enabled |
| Identity | DIS model/firmware; no confirmed serial; host enrollment fingerprint |
| Battery | 150 mAh; **10–14 h continuous capture**; daily charge |

### Services and protocol

| Service | UUID | Direction | Notes |
|---|---|---|---|
| Battery | `0x180F` (`0x2A19`) | read/notify | Percent |
| Device Information | `0x180A` | read | Model, firmware, hardware |
| Audio | `19B10000-E8F2-537E-4F6C-D104768A1214` | notify | Audio packets |
| Audio Data | `19B10001-E8F2-537E-4F6C-D104768A1214` | notify | 3-byte header + payload |
| Codec | `19B10002-E8F2-537E-4F6C-D104768A1214` | read/write | Codec selector |

Codec values: `0` PCM 16 kHz, `1` PCM 8 kHz, `10` Mu-law 16 kHz, `11` Mu-law
8 kHz, `20` Opus 16 kHz. Each packet carries 160 samples; the 3-byte header is
a 16-bit packet number plus a 1-byte index. Payloads are little-endian and may
fragment across notifications.

- **Signals:** `audio.segment.v1` (metadata + CID), `device.state.v1`.
- **Today:** the OMI app/backend does VAD, transcription, and diarization. The
  device protocol exposes audio, battery, and DIS; the IMU/button are not
  confirmed as third-party BLE services.
- **Buffer, do not stream:** 512 MB NAND; 16 kHz Opus at 16–24 kbps is about
  7–11 MB/hour, 70–110 MB per 10-hour day. VAD on the host; on-device VAD is a
  firmware change.

## KZ AZ09

| Field | Value |
|---|---|
| Kind | `audio.earbuds` |
| Transport | Bluetooth Classic 5.2; A2DP sink, HFP mic, AVRCP controls |
| Identity | Bluetooth MAC / host pairing record; no stable sensor identity |
| Battery | 50 mAh per hook, ~6 h playback; 800 mAh case = 8–10 charges |
| Constraints | No BLE GATT data channel; no sensors; one host at a time |

- **Signals:** `device.state.v1` battery/link only, and only if the host can
  read it; do not depend on a standard battery service.
- **Commands:** button presses arrive as AVRCP/HFP events on the host, not BLE
  GATT. Map them to media/JARVIS actions through MPRIS (Linux) or MediaSession
  (Android).
- **Today:** works as an audio route on stock Android; mainline BT audio is
  still WIP. It is not a telemetry device and should not become one.

## CAT S22

| Field | Value |
|---|---|
| Kind | `edge.phone` |
| SoC | Qualcomm QM215, 2 GB RAM, 16 GB storage, microSD |
| Display | 3.4" 480×640 inner, 128×128 outer |
| Radios | LTE (modem WIP), Wi-Fi, Bluetooth/BLE, USB |
| Sensors | Mic, keypad, lid/hall, camera (WIP), GNSS (WIP), ALS/proximity/pressure (drivers WIP), NFC (WIP) |
| Battery | 3000 mAh; mainline 8–15 h as a screen-off bridge; stock Android 1–2 days |
| Constraints | **No suspend**; modem and cameras are the last frontier; thermal/power policy WIP |

- **Role:** BLE central and bridge, Wi-Fi host, audio output, USB storage, and
  eventually the Mycelium edge peer. It is a central, not a GATT peripheral.
- **Signals:** `audio.segment.v1` from the mic, `location.fix.v1` (WIP),
  `device.state.v1` battery/thermal/link, `presence.state.v1` via BLE/Wi-Fi
  scans, `body.steps.v1` if the mainline sensor stack is qualified.
- **Today:** speakers, BLE, Wi-Fi, USB disk I/O, battery, lid, keypad, and mic
  work. Modem and cameras remain WIP. Suspend is the gate for always-on.
- **Mycelium:** run an edge profile, not a desktop gossip node: long
  anti-entropy intervals, batched sync, no inbound relay, wake on demand.

## Profile format

The simulator will read a machine-readable profile. The shape is:

```toml
[device.pinetime]
kind = "wearable.watch"
model = "PineTime"
transport = "ble-gatt"
identity = "dis.serial"
battery_mah = 175
battery_life_hours = { notifications = 120, continuous_hr = 36 }
flash_kb = 4096
services = ["0x180A", "0x180D", "0x180F", "0x1805", "0x1811", "0x1802"]
signals = ["body.hr.v1", "body.motion.v1", "body.steps.v1", "device.state.v1"]
constraints = { connections = 1, att_payload_bytes = 23, dle = false }
```

Profiles are inputs to the simulator and the Spore manifests. They do not grant
anything; the signed manifest and Mycelium grant still decide what a Spore may
observe or write.

## Non-goals

- A profile is not a device driver. The Spore owns the protocol.
- A profile is not a capability grant.
- A BLE address is not a device identity.
- A battery estimate is not a measurement.
- A device that only exposes data through a vendor app is not “supported” until
  the protocol is mapped.
