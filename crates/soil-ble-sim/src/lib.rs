//! Deterministic virtual BLE backend for `soil:ble`.
//!
//! The simulator is not a Bluetooth emulator. It is a GATT-shaped source of
//! truth with a virtual clock, battery drain, scripted packet loss, disconnects,
//! stale reads, and a lease contender. That is enough to develop the host and
//! the first Spores without a radio, and to reproduce a bad day on demand.
//!
//! Every operation advances or observes `tick`; nothing reads the wall clock.
//! A test can therefore assert exact values instead of sleeping.

pub mod signal;

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Mutex;

use anyhow::{Result, anyhow, bail};
use soil_ble_api::{Backend, BleEvent, CharInfo, DeviceInfo, ServiceInfo};

const DIS: &str = "0000180a-0000-1000-8000-00805f9b34fb";
const HRS: &str = "0000180d-0000-1000-8000-00805f9b34fb";
const BAS: &str = "0000180f-0000-1000-8000-00805f9b34fb";
const ANS: &str = "00001811-0000-1000-8000-00805f9b34fb";
const IAS: &str = "00001802-0000-1000-8000-00805f9b34fb";
const CTS: &str = "00001805-0000-1000-8000-00805f9b34fb";

const DIS_MODEL: &str = "00002a24-0000-1000-8000-00805f9b34fb";
const DIS_SERIAL: &str = "00002a25-0000-1000-8000-00805f9b34fb";
const DIS_FIRMWARE: &str = "00002a26-0000-1000-8000-00805f9b34fb";
const DIS_MANUFACTURER: &str = "00002a29-0000-1000-8000-00805f9b34fb";
const HR_MEASUREMENT: &str = "00002a37-0000-1000-8000-00805f9b34fb";
const BATTERY_LEVEL: &str = "00002a19-0000-1000-8000-00805f9b34fb";
const ALERT_NEW: &str = "00002a46-0000-1000-8000-00805f9b34fb";
const ALERT_LEVEL: &str = "00002a06-0000-1000-8000-00805f9b34fb";

const PINETIME_MOTION_SERVICE: &str = "00030000-78fc-48fe-8e23-433b3a1942d0";
const PINETIME_STEPS: &str = "00030001-78fc-48fe-8e23-433b3a1942d0";
const PINETIME_MOTION: &str = "00030002-78fc-48fe-8e23-433b3a1942d0";
const PINETIME_MUSIC_SERVICE: &str = "00000000-78fc-48fe-8e23-433b3a1942d0";
const PINETIME_MUSIC_EVENT: &str = "00000001-78fc-48fe-8e23-433b3a1942d0";
const PINETIME_MUSIC_STATUS: &str = "00000002-78fc-48fe-8e23-433b3a1942d0";
const PINETIME_MUSIC_ARTIST: &str = "00000003-78fc-48fe-8e23-433b3a1942d0";
const PINETIME_MUSIC_TRACK: &str = "00000004-78fc-48fe-8e23-433b3a1942d0";
const PINETIME_MUSIC_ALBUM: &str = "00000005-78fc-48fe-8e23-433b3a1942d0";
const PINETIME_MUSIC_POSITION: &str = "00000006-78fc-48fe-8e23-433b3a1942d0";
const PINETIME_MUSIC_LENGTH: &str = "00000007-78fc-48fe-8e23-433b3a1942d0";
const PINETIME_MUSIC_TRACK_NUMBER: &str = "00000008-78fc-48fe-8e23-433b3a1942d0";
const PINETIME_MUSIC_TRACK_TOTAL: &str = "00000009-78fc-48fe-8e23-433b3a1942d0";
const PINETIME_MUSIC_SPEED: &str = "0000000a-78fc-48fe-8e23-433b3a1942d0";
const PINETIME_MUSIC_REPEAT: &str = "0000000b-78fc-48fe-8e23-433b3a1942d0";
const PINETIME_MUSIC_SHUFFLE: &str = "0000000c-78fc-48fe-8e23-433b3a1942d0";
const PINETIME_NOTIFICATION_EVENT: &str = "00020001-78fc-48fe-8e23-433b3a1942d0";
const RING_SERVICE: &str = "0000fff0-0000-1000-8000-00805f9b34fb";
const RING_MEASUREMENT: &str = "0000fff1-0000-1000-8000-00805f9b34fb";
const OMI_AUDIO_SERVICE: &str = "19b10000-e8f2-537e-4f6c-d104768a1214";
const OMI_AUDIO_DATA: &str = "19b10001-e8f2-537e-4f6c-d104768a1214";
const OMI_CODEC: &str = "19b10002-e8f2-537e-4f6c-d104768a1214";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Source {
    Static,
    HeartRate,
    Battery,
    Steps,
    Motion,
    MusicEvent,
    CallResponse,
    RingMeasurement,
    OmiAudio,
}

#[derive(Debug, Clone)]
struct SimCharacteristic {
    uuid: String,
    readable: bool,
    writable: bool,
    notifiable: bool,
    value: Vec<u8>,
    source: Source,
}

#[derive(Debug, Clone)]
struct SimService {
    uuid: String,
    characteristics: Vec<SimCharacteristic>,
}

#[derive(Debug, Clone, Default)]
pub struct Faults {
    /// Drop all notifications on ticks where `tick % drop_every == 0`.
    pub drop_every: Option<u64>,
    /// Emit a disconnect once the link reaches this tick.
    pub disconnect_at: Option<u64>,
    /// Reads after this tick fail with a stale error.
    pub stale_after: Option<u64>,
    /// Characteristic UUIDs whose reads are denied.
    pub denied: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct SimDevice {
    pub id: String,
    pub name: String,
    pub rssi: i16,
    pub battery_percent: f32,
    pub drain_per_tick: f32,
    pub faults: Faults,
    services: Vec<SimService>,
}

struct SimState {
    devices: Vec<SimDevice>,
    link: Option<usize>,
    subscribed: HashSet<String>,
    events: VecDeque<BleEvent>,
    writes: HashMap<String, Vec<u8>>,
    tick: u64,
}

pub struct SimBackend {
    state: Mutex<SimState>,
}

impl SimBackend {
    pub fn builtin() -> Result<Self> {
        Ok(Self::with_devices(vec![pine_time(), ring(), omi()]))
    }

    pub fn with_devices(devices: Vec<SimDevice>) -> Self {
        Self {
            state: Mutex::new(SimState {
                devices,
                link: None,
                subscribed: HashSet::new(),
                events: VecDeque::new(),
                writes: HashMap::new(),
                tick: 0,
            }),
        }
    }

    /// Current virtual tick. Tests use this instead of sleeping.
    pub fn tick(&self) -> u64 {
        self.state.lock().expect("sim state").tick
    }

    /// Battery of one virtual device, for assertions.
    pub fn battery(&self, id: &str) -> Option<f32> {
        self.state
            .lock()
            .expect("sim state")
            .devices
            .iter()
            .find(|device| device.id == id)
            .map(|device| device.battery_percent)
    }

    /// Last value written to a characteristic, for command-channel tests.
    pub fn last_write(&self, uuid: &str) -> Option<Vec<u8>> {
        self.state
            .lock()
            .expect("sim state")
            .writes
            .get(uuid)
            .cloned()
    }
}

impl Backend for SimBackend {
    fn scan(&mut self, _timeout_ms: u64) -> Result<Vec<DeviceInfo>> {
        let state = self.state.lock().expect("sim state");
        Ok(state
            .devices
            .iter()
            .map(|device| DeviceInfo {
                id: device.id.clone(),
                name: Some(device.name.clone()),
                rssi: Some(device.rssi),
            })
            .collect())
    }

    fn connect(&mut self, id: &str) -> Result<()> {
        let mut state = self.state.lock().expect("sim state");
        let index = state
            .devices
            .iter()
            .position(|device| device.id.eq_ignore_ascii_case(id))
            .ok_or_else(|| anyhow!("unknown simulated device {id}"))?;
        state.link = Some(index);
        state.subscribed.clear();
        state.events.clear();
        state.tick = 0;
        Ok(())
    }

    fn disconnect(&mut self) -> Result<()> {
        let mut state = self.state.lock().expect("sim state");
        state.link = None;
        state.subscribed.clear();
        state.events.clear();
        Ok(())
    }

    fn services(&self) -> Result<Vec<ServiceInfo>> {
        let state = self.state.lock().expect("sim state");
        let device = linked(&state)?;
        Ok(device
            .services
            .iter()
            .map(|service| ServiceInfo {
                uuid: service.uuid.clone(),
                characteristics: service
                    .characteristics
                    .iter()
                    .map(|characteristic| CharInfo {
                        uuid: characteristic.uuid.clone(),
                        readable: characteristic.readable,
                        writable: characteristic.writable,
                        notifiable: characteristic.notifiable,
                    })
                    .collect(),
            })
            .collect())
    }

    fn read(&self, uuid: &str) -> Result<Vec<u8>> {
        let state = self.state.lock().expect("sim state");
        let device = linked(&state)?;
        let characteristic = find_characteristic(device, uuid)?;
        if !characteristic.readable {
            bail!("characteristic {uuid} is not readable");
        }
        if device
            .faults
            .denied
            .iter()
            .any(|denied| denied.eq_ignore_ascii_case(uuid))
        {
            bail!("characteristic {uuid} is denied");
        }
        if device
            .faults
            .stale_after
            .is_some_and(|after| state.tick > after)
        {
            bail!("characteristic {uuid} is stale");
        }
        Ok(current_value(device, characteristic, state.tick))
    }

    fn write(&self, uuid: &str, data: &[u8]) -> Result<()> {
        let mut state = self.state.lock().expect("sim state");
        let (key, is_static) = {
            let device = linked_mut(&mut state)?;
            let characteristic = find_characteristic(device, uuid)?;
            if !characteristic.writable {
                bail!("characteristic {uuid} is not writable");
            }
            (
                characteristic.uuid.clone(),
                characteristic.source == Source::Static,
            )
        };
        if data.len() > 512 {
            bail!("write exceeds 512 bytes");
        }
        if is_static {
            let device = linked_mut(&mut state)?;
            if let Some(characteristic) = device
                .services
                .iter_mut()
                .flat_map(|service| service.characteristics.iter_mut())
                .find(|characteristic| characteristic.uuid == key)
            {
                characteristic.value = data.to_vec();
            }
        }
        state.writes.insert(key, data.to_vec());
        Ok(())
    }

    fn subscribe(&self, uuid: &str) -> Result<()> {
        let mut state = self.state.lock().expect("sim state");
        let canonical = {
            let device = linked(&state)?;
            let characteristic = find_characteristic(device, uuid)?;
            if !characteristic.notifiable {
                bail!("characteristic {uuid} is not notifiable");
            }
            characteristic.uuid.clone()
        };
        // A real peripheral does not replay its current value merely because a
        // central subscribed. Notifications arrive on change; tests drive the
        // virtual clock with `next_event`.
        state.subscribed.insert(canonical);
        Ok(())
    }

    fn next_event(&self, _timeout_ms: u64) -> Result<Option<BleEvent>> {
        let mut state = self.state.lock().expect("sim state");
        if let Some(event) = state.events.pop_front() {
            return Ok(Some(event));
        }
        let link = state.link.ok_or_else(|| anyhow!("not connected"))?;
        state.tick = state.tick.saturating_add(1);
        let tick = state.tick;

        let device = &mut state.devices[link];
        device.battery_percent = (device.battery_percent - device.drain_per_tick).max(0.0);
        let disconnect_at = device.faults.disconnect_at;
        let drop_every = device.faults.drop_every;

        if disconnect_at.is_some_and(|at| tick >= at) {
            state.link = None;
            state.subscribed.clear();
            state.events.clear();
            return Ok(Some(BleEvent::Disconnected {
                reason: format!("simulated disconnect at tick {tick}"),
            }));
        }

        if drop_every.is_some_and(|every| every > 0 && tick % every == 0) {
            return Ok(None);
        }

        let device = &state.devices[link];
        let mut generated = Vec::new();
        for service in &device.services {
            for characteristic in &service.characteristics {
                if !state.subscribed.contains(&characteristic.uuid) {
                    continue;
                }
                if characteristic.source == Source::Battery && tick % 10 != 0 {
                    continue;
                }
                generated.push(BleEvent::Notification {
                    char_uuid: characteristic.uuid.clone(),
                    data: notify_value(device, characteristic, tick),
                });
            }
        }
        let first = generated.first().cloned();
        state.events.extend(generated.into_iter().skip(1));
        Ok(first)
    }
}

fn linked(state: &SimState) -> Result<&SimDevice> {
    state
        .link
        .and_then(|index| state.devices.get(index))
        .ok_or_else(|| anyhow!("not connected"))
}

fn linked_mut(state: &mut SimState) -> Result<&mut SimDevice> {
    state
        .link
        .and_then(|index| state.devices.get_mut(index))
        .ok_or_else(|| anyhow!("not connected"))
}

fn find_characteristic<'a>(device: &'a SimDevice, uuid: &str) -> Result<&'a SimCharacteristic> {
    device
        .services
        .iter()
        .flat_map(|service| service.characteristics.iter())
        .find(|characteristic| characteristic.uuid.eq_ignore_ascii_case(uuid))
        .ok_or_else(|| anyhow!("unknown characteristic {uuid}"))
}

fn current_value(device: &SimDevice, characteristic: &SimCharacteristic, tick: u64) -> Vec<u8> {
    match characteristic.source {
        Source::Static => characteristic.value.clone(),
        Source::HeartRate => heart_rate(tick),
        Source::Battery => vec![battery(device)],
        Source::Steps => steps(tick),
        Source::Motion => motion(tick),
        Source::MusicEvent => music_event(tick),
        Source::CallResponse => call_response(tick),
        Source::RingMeasurement => ring_measurement(device, tick),
        Source::OmiAudio => omi_audio(tick),
    }
}

fn notify_value(device: &SimDevice, characteristic: &SimCharacteristic, tick: u64) -> Vec<u8> {
    current_value(device, characteristic, tick)
}

fn battery(device: &SimDevice) -> u8 {
    device.battery_percent.round().clamp(0.0, 100.0) as u8
}

fn heart_rate(tick: u64) -> Vec<u8> {
    // InfiniTime HRS: flags=0 means the measurement is the following uint8.
    let bpm: u8 = 60 + (tick % 30) as u8;
    vec![0x00, bpm]
}

fn steps(tick: u64) -> Vec<u8> {
    // InfiniTime motion service: uint32 little-endian step count.
    ((tick / 2) as u32).to_le_bytes().to_vec()
}

fn motion(tick: u64) -> Vec<u8> {
    // InfiniTime motion service: three int16 little-endian axes.
    let phase = (tick % 12) as i16;
    let mut out = Vec::with_capacity(6);
    for value in [phase * 7, -phase * 3, 1000 - phase * 2] {
        out.extend(value.to_le_bytes());
    }
    out
}

fn music_event(tick: u64) -> Vec<u8> {
    // InfiniTime event codes; the watch sends these to the phone.
    const EVENTS: [u8; 5] = [0x00, 0x01, 0x03, 0x04, 0x05];
    vec![EVENTS[(tick as usize) % EVENTS.len()]]
}

fn call_response(tick: u64) -> Vec<u8> {
    // Reject / answer / mute, matching InfiniTime's IncomingCallResponses.
    vec![(tick % 3) as u8]
}

fn ring_measurement(device: &SimDevice, tick: u64) -> Vec<u8> {
    // Synthetic until the QRing protocol is mapped: HR, SpO2, battery.
    let bpm: u8 = 60 + (tick % 30) as u8;
    let spo2: u8 = 97;
    vec![bpm, spo2, battery(device)]
}

fn omi_audio(tick: u64) -> Vec<u8> {
    // 3-byte packet header + 160 bytes of payload, matching the OMI framing.
    let packet = tick as u16;
    let mut out = Vec::with_capacity(163);
    out.push(packet as u8);
    out.push((packet >> 8) as u8);
    out.push(0);
    out.extend((0..160).map(|index| ((tick as usize + index) % 251) as u8));
    out
}

fn service(uuid: &str, characteristics: Vec<SimCharacteristic>) -> SimService {
    SimService {
        uuid: uuid.to_ascii_lowercase(),
        characteristics,
    }
}

fn characteristic(
    uuid: &str,
    readable: bool,
    writable: bool,
    notifiable: bool,
    value: Vec<u8>,
    source: Source,
) -> SimCharacteristic {
    SimCharacteristic {
        uuid: uuid.to_ascii_lowercase(),
        readable,
        writable,
        notifiable,
        value,
        source,
    }
}

fn pine_time() -> SimDevice {
    SimDevice {
        id: "sim-pinetime".to_string(),
        name: "PineTime".to_string(),
        rssi: -55,
        battery_percent: 82.0,
        drain_per_tick: 0.01,
        faults: Faults::default(),
        services: vec![
            service(
                DIS,
                vec![
                    characteristic(
                        DIS_MANUFACTURER,
                        true,
                        false,
                        false,
                        b"Pine64".to_vec(),
                        Source::Static,
                    ),
                    characteristic(
                        DIS_MODEL,
                        true,
                        false,
                        false,
                        b"PineTime".to_vec(),
                        Source::Static,
                    ),
                    characteristic(
                        DIS_SERIAL,
                        true,
                        false,
                        false,
                        b"sim-pinetime-0001".to_vec(),
                        Source::Static,
                    ),
                    characteristic(
                        DIS_FIRMWARE,
                        true,
                        false,
                        false,
                        b"InfiniTime 1.16.0".to_vec(),
                        Source::Static,
                    ),
                ],
            ),
            service(
                HRS,
                vec![characteristic(
                    HR_MEASUREMENT,
                    true,
                    false,
                    true,
                    Vec::new(),
                    Source::HeartRate,
                )],
            ),
            service(
                BAS,
                vec![characteristic(
                    BATTERY_LEVEL,
                    true,
                    false,
                    true,
                    Vec::new(),
                    Source::Battery,
                )],
            ),
            service(
                ANS,
                vec![
                    characteristic(ALERT_NEW, false, true, false, Vec::new(), Source::Static),
                    characteristic(
                        PINETIME_NOTIFICATION_EVENT,
                        false,
                        false,
                        true,
                        Vec::new(),
                        Source::CallResponse,
                    ),
                ],
            ),
            service(
                IAS,
                vec![characteristic(
                    ALERT_LEVEL,
                    false,
                    true,
                    false,
                    Vec::new(),
                    Source::Static,
                )],
            ),
            service(
                CTS,
                vec![characteristic(
                    "00002a2b-0000-1000-8000-00805f9b34fb",
                    true,
                    true,
                    false,
                    Vec::new(),
                    Source::Static,
                )],
            ),
            service(
                PINETIME_MOTION_SERVICE,
                vec![
                    characteristic(PINETIME_STEPS, true, false, true, Vec::new(), Source::Steps),
                    characteristic(
                        PINETIME_MOTION,
                        true,
                        false,
                        true,
                        Vec::new(),
                        Source::Motion,
                    ),
                ],
            ),
            service(
                PINETIME_MUSIC_SERVICE,
                vec![
                    characteristic(
                        PINETIME_MUSIC_EVENT,
                        false,
                        false,
                        true,
                        Vec::new(),
                        Source::MusicEvent,
                    ),
                    characteristic(
                        PINETIME_MUSIC_STATUS,
                        true,
                        true,
                        false,
                        vec![0],
                        Source::Static,
                    ),
                    characteristic(
                        PINETIME_MUSIC_ARTIST,
                        true,
                        true,
                        false,
                        Vec::new(),
                        Source::Static,
                    ),
                    characteristic(
                        PINETIME_MUSIC_TRACK,
                        true,
                        true,
                        false,
                        Vec::new(),
                        Source::Static,
                    ),
                    characteristic(
                        PINETIME_MUSIC_ALBUM,
                        true,
                        true,
                        false,
                        Vec::new(),
                        Source::Static,
                    ),
                    characteristic(
                        PINETIME_MUSIC_POSITION,
                        true,
                        true,
                        false,
                        vec![0; 4],
                        Source::Static,
                    ),
                    characteristic(
                        PINETIME_MUSIC_LENGTH,
                        true,
                        true,
                        false,
                        vec![0; 4],
                        Source::Static,
                    ),
                    characteristic(
                        PINETIME_MUSIC_TRACK_NUMBER,
                        true,
                        true,
                        false,
                        vec![0; 4],
                        Source::Static,
                    ),
                    characteristic(
                        PINETIME_MUSIC_TRACK_TOTAL,
                        true,
                        true,
                        false,
                        vec![0; 4],
                        Source::Static,
                    ),
                    characteristic(
                        PINETIME_MUSIC_SPEED,
                        true,
                        true,
                        false,
                        vec![0; 4],
                        Source::Static,
                    ),
                    characteristic(
                        PINETIME_MUSIC_REPEAT,
                        true,
                        true,
                        false,
                        vec![0],
                        Source::Static,
                    ),
                    characteristic(
                        PINETIME_MUSIC_SHUFFLE,
                        true,
                        true,
                        false,
                        vec![0],
                        Source::Static,
                    ),
                ],
            ),
        ],
    }
}

fn ring() -> SimDevice {
    SimDevice {
        id: "sim-ring".to_string(),
        name: "COLMI Ring 2".to_string(),
        rssi: -62,
        battery_percent: 74.0,
        drain_per_tick: 0.004,
        faults: Faults::default(),
        services: vec![
            service(
                HRS,
                vec![characteristic(
                    HR_MEASUREMENT,
                    true,
                    false,
                    true,
                    Vec::new(),
                    Source::HeartRate,
                )],
            ),
            service(
                BAS,
                vec![characteristic(
                    BATTERY_LEVEL,
                    true,
                    false,
                    true,
                    Vec::new(),
                    Source::Battery,
                )],
            ),
            service(
                RING_SERVICE,
                vec![characteristic(
                    RING_MEASUREMENT,
                    true,
                    false,
                    true,
                    Vec::new(),
                    Source::RingMeasurement,
                )],
            ),
        ],
    }
}

fn omi() -> SimDevice {
    SimDevice {
        id: "sim-omi".to_string(),
        name: "Omi".to_string(),
        rssi: -58,
        battery_percent: 63.0,
        drain_per_tick: 0.05,
        faults: Faults::default(),
        services: vec![
            service(
                DIS,
                vec![
                    characteristic(
                        DIS_MANUFACTURER,
                        true,
                        false,
                        false,
                        b"Based Hardware".to_vec(),
                        Source::Static,
                    ),
                    characteristic(
                        DIS_MODEL,
                        true,
                        false,
                        false,
                        b"Omi".to_vec(),
                        Source::Static,
                    ),
                    characteristic(
                        DIS_FIRMWARE,
                        true,
                        false,
                        false,
                        b"1.0.3".to_vec(),
                        Source::Static,
                    ),
                ],
            ),
            service(
                BAS,
                vec![characteristic(
                    BATTERY_LEVEL,
                    true,
                    false,
                    true,
                    Vec::new(),
                    Source::Battery,
                )],
            ),
            service(
                OMI_AUDIO_SERVICE,
                vec![
                    characteristic(
                        OMI_AUDIO_DATA,
                        false,
                        false,
                        true,
                        Vec::new(),
                        Source::OmiAudio,
                    ),
                    characteristic(OMI_CODEC, true, true, false, vec![20], Source::Static),
                ],
            ),
        ],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn backend() -> SimBackend {
        SimBackend::builtin().expect("sim")
    }

    #[test]
    fn scan_finds_builtin_devices() {
        let mut sim = backend();
        let devices = sim.scan(100).expect("scan");
        let names: Vec<_> = devices
            .iter()
            .filter_map(|device| device.name.as_deref())
            .collect();
        assert!(names.contains(&"PineTime"));
        assert!(names.contains(&"COLMI Ring 2"));
        assert!(names.contains(&"Omi"));
    }

    #[test]
    fn connect_read_and_subscribe() {
        let mut sim = backend();
        sim.connect("sim-pinetime").expect("connect");
        let services = sim.services().expect("services");
        assert!(services.iter().any(|service| service.uuid == HRS));
        let serial = sim.read(DIS_SERIAL).expect("serial");
        assert_eq!(serial, b"sim-pinetime-0001");
        let battery = sim.read(BATTERY_LEVEL).expect("battery");
        assert_eq!(battery.len(), 1);
        sim.subscribe(HR_MEASUREMENT).expect("subscribe");
        let event = sim.next_event(100).expect("event").expect("some");
        match event {
            BleEvent::Notification { char_uuid, data } => {
                assert_eq!(char_uuid, HR_MEASUREMENT);
                assert_eq!(data.len(), 2, "InfiniTime sends flags + uint8 HR");
                assert_eq!(data[1], 61);
            }
            BleEvent::Disconnected { .. } => panic!("unexpected disconnect"),
        }
    }

    #[test]
    fn packet_loss_skips_a_tick() {
        let mut sim = SimBackend::with_devices(vec![{
            let mut device = pine_time();
            device.faults.drop_every = Some(2);
            device
        }]);
        sim.connect("sim-pinetime").expect("connect");
        sim.subscribe(HR_MEASUREMENT).expect("subscribe");
        // Tick 1 succeeds, tick 2 drops, tick 3 succeeds.
        assert!(sim.next_event(100).expect("tick 1").is_some());
        assert!(sim.next_event(100).expect("tick 2").is_none());
        assert!(sim.next_event(100).expect("tick 3").is_some());
    }

    #[test]
    fn disconnect_is_an_event() {
        let mut sim = SimBackend::with_devices(vec![{
            let mut device = pine_time();
            device.faults.disconnect_at = Some(2);
            device
        }]);
        sim.connect("sim-pinetime").expect("connect");
        sim.subscribe(HR_MEASUREMENT).expect("subscribe");
        let _ = sim.next_event(100).expect("tick 1");
        let event = sim.next_event(100).expect("tick 2").expect("event");
        assert!(matches!(event, BleEvent::Disconnected { .. }));
        assert!(sim.next_event(100).is_err());
    }

    #[test]
    fn stale_and_denied_reads_fail() {
        let mut sim = SimBackend::with_devices(vec![{
            let mut device = pine_time();
            device.faults.stale_after = Some(0);
            device.faults.denied.push(DIS_MODEL.to_string());
            device
        }]);
        sim.connect("sim-pinetime").expect("connect");
        // Denial takes precedence and is an error, never a zero.
        assert!(sim.read(DIS_MODEL).is_err());
        // Advance the virtual clock past the stale bound.
        let _ = sim.next_event(100);
        assert!(sim.read(DIS_SERIAL).is_err());
    }

    #[test]
    fn omi_audio_has_header_and_payload() {
        let mut sim = backend();
        sim.connect("sim-omi").expect("connect");
        sim.subscribe(OMI_AUDIO_DATA).expect("subscribe");
        let event = sim.next_event(100).expect("event").expect("some");
        match event {
            BleEvent::Notification { char_uuid, data } => {
                assert_eq!(char_uuid, OMI_AUDIO_DATA);
                assert_eq!(data.len(), 163);
                assert_eq!(data[2], 0);
            }
            BleEvent::Disconnected { .. } => panic!("unexpected disconnect"),
        }
    }

    #[test]
    fn pine_time_exposes_the_real_infinitime_services() {
        let mut sim = backend();
        sim.connect("sim-pinetime").expect("connect");
        let services = sim.services().expect("services");
        let uuids: Vec<_> = services
            .iter()
            .map(|service| service.uuid.as_str())
            .collect();
        for expected in [
            HRS,
            BAS,
            ANS,
            IAS,
            CTS,
            PINETIME_MOTION_SERVICE,
            PINETIME_MUSIC_SERVICE,
        ] {
            assert!(uuids.contains(&expected), "missing service {expected}");
        }
        let motion = services
            .iter()
            .find(|service| service.uuid == PINETIME_MOTION_SERVICE)
            .expect("motion service");
        assert!(
            motion
                .characteristics
                .iter()
                .any(|c| c.uuid == PINETIME_STEPS)
        );
        assert!(
            motion
                .characteristics
                .iter()
                .any(|c| c.uuid == PINETIME_MOTION)
        );
    }

    #[test]
    fn steps_and_motion_are_distinct_values() {
        let mut sim = backend();
        sim.connect("sim-pinetime").expect("connect");
        let steps = sim.read(PINETIME_STEPS).expect("steps");
        assert_eq!(steps.len(), 4, "uint32 step count");
        assert_eq!(u32::from_le_bytes(steps.as_slice().try_into().unwrap()), 0);
        let motion = sim.read(PINETIME_MOTION).expect("motion");
        assert_eq!(motion.len(), 6, "three int16 axes");
        // A step count is not an axis sum and the simulator never fabricates a
        // combined total.
        assert_ne!(steps.len(), motion.len());
    }

    #[test]
    fn notification_and_music_writes_reach_the_device() {
        let mut sim = backend();
        sim.connect("sim-pinetime").expect("connect");

        sim.write(ALERT_NEW, &[0x03, 0x01, b'h', b'i'])
            .expect("ANS write");
        assert_eq!(
            sim.last_write(ALERT_NEW),
            Some(vec![0x03, 0x01, b'h', b'i'])
        );

        sim.write(PINETIME_MUSIC_STATUS, &[1])
            .expect("music status");
        assert_eq!(sim.read(PINETIME_MUSIC_STATUS).expect("status"), vec![1]);
        sim.write(PINETIME_MUSIC_ARTIST, b"Autechre")
            .expect("artist");
        assert_eq!(
            sim.read(PINETIME_MUSIC_ARTIST).expect("artist"),
            b"Autechre"
        );
    }

    #[test]
    fn replay_is_deterministic_and_golden() {
        fn trace() -> Vec<(u64, String, Vec<u8>)> {
            let mut sim = backend();
            sim.connect("sim-pinetime").expect("connect");
            for uuid in [HR_MEASUREMENT, PINETIME_STEPS, PINETIME_MOTION] {
                sim.subscribe(uuid).expect("subscribe");
            }
            let mut out = Vec::new();
            for _ in 0..6 {
                match sim.next_event(100).expect("event") {
                    Some(BleEvent::Notification { char_uuid, data }) => {
                        out.push((sim.tick(), char_uuid, data));
                    }
                    Some(BleEvent::Disconnected { reason }) => panic!("disconnect: {reason}"),
                    None => panic!("missing event"),
                }
            }
            out
        }

        let first = trace();
        assert_eq!(first, trace(), "same script must replay byte-for-byte");
        assert_eq!(
            first,
            vec![
                (1, HR_MEASUREMENT.to_string(), vec![0x00, 61]),
                (1, PINETIME_STEPS.to_string(), vec![0, 0, 0, 0]),
                (
                    1,
                    PINETIME_MOTION.to_string(),
                    vec![7, 0, 0xfd, 0xff, 0xe6, 0x03]
                ),
                (2, HR_MEASUREMENT.to_string(), vec![0x00, 62]),
                (2, PINETIME_STEPS.to_string(), vec![1, 0, 0, 0]),
                (
                    2,
                    PINETIME_MOTION.to_string(),
                    vec![14, 0, 0xfa, 0xff, 0xe4, 0x03]
                ),
            ]
        );
    }

    #[test]
    fn battery_drain_clamps_at_zero() {
        let mut sim = SimBackend::with_devices(vec![{
            let mut device = pine_time();
            device.battery_percent = 0.02;
            device.drain_per_tick = 0.01;
            device
        }]);
        sim.connect("sim-pinetime").expect("connect");
        for _ in 0..5 {
            let _ = sim.next_event(100);
        }
        assert_eq!(sim.battery("sim-pinetime"), Some(0.0));
    }
    #[test]
    fn battery_drains_deterministically() {
        let mut sim = backend();
        sim.connect("sim-pinetime").expect("connect");
        let before = sim.battery("sim-pinetime").expect("battery");
        let _ = sim.next_event(100);
        let after = sim.battery("sim-pinetime").expect("battery");
        assert!(after < before);
        assert_eq!(sim.tick(), 1);
    }

    #[test]
    fn stale_reads_are_unknown_never_zero() {
        let mut sim = SimBackend::with_devices(vec![{
            let mut device = pine_time();
            device.faults.stale_after = Some(0);
            device
        }]);
        sim.connect("sim-pinetime").expect("connect");
        let _ = sim.next_event(100);
        let result = sim.read(PINETIME_STEPS);
        assert!(result.is_err(), "stale must not become a zero reading");
        assert!(result.unwrap_err().to_string().contains("stale"));
    }

    #[test]
    fn disconnect_releases_the_gatt_tree() {
        let mut sim = backend();
        sim.connect("sim-pinetime").expect("connect");
        assert!(sim.services().is_ok());
        sim.disconnect().expect("release");
        assert!(sim.services().is_err(), "a released link has no GATT tree");
    }

    #[test]
    fn writes_are_recorded() {
        let mut sim = backend();
        sim.connect("sim-pinetime").expect("connect");
        sim.write(ALERT_LEVEL, &[2]).expect("write");
        assert_eq!(sim.last_write(ALERT_LEVEL), Some(vec![2]));
    }
}
