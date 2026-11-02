//! COLMI ring protocol Spore: owns framing and command semantics.
//!
//! Everything the ring says is opaque bytes at the `soil:ble` boundary. The
//! 16-byte `[cmd][14 payload][checksum]` framing and the `0x80` error bit are
//! decoded here, not in the host. The Spore has no radio, path, socket or clock
//! authority beyond the BLE capability.

wit_bindgen::generate!({ path: "wit", world: "ring-spore", generate_all });

use soil::ble::ble::{self, Error as BleError};
use soil::ble::types::Event;

#[allow(dead_code)]
const DIS_FIRMWARE: &str = "00002a26-0000-1000-8000-00805f9b34fb";
#[allow(dead_code)]
const DIS_HARDWARE: &str = "00002a27-0000-1000-8000-00805f9b34fb";
#[allow(dead_code)]
const DIS_SERIAL: &str = "00002a25-0000-1000-8000-00805f9b34fb";
#[allow(dead_code)]
const V1_CHANNEL: &str = "6e40fff0-b5a3-f393-e0a9-e50e24dcca9e";
const V1_WRITE: &str = "6e400002-b5a3-f393-e0a9-e50e24dcca9e";
const V1_NOTIFY: &str = "6e400003-b5a3-f393-e0a9-e50e24dcca9e";

struct Component;

export!(Component);

fn ble_err(e: BleError) -> String {
    format!("{e:?}")
}

#[allow(dead_code)]
fn utf8(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .trim_end_matches('\0')
        .trim()
        .to_string()
}

/// One ring frame, once decoded.
struct Frame<'a> {
    cmd: u8,
    error: bool,
    payload: &'a [u8],
    checksum_ok: bool,
}

fn decode(raw: &[u8]) -> Option<Frame<'_>> {
    if raw.len() != 16 {
        return None;
    }
    let sum: u8 = raw[..15].iter().fold(0u8, |a, b| a.wrapping_add(*b));
    Some(Frame {
        cmd: raw[0] & 0x7f,
        error: raw[0] & 0x80 != 0,
        payload: &raw[1..15],
        checksum_ok: sum == raw[15],
    })
}

impl Guest for Component {
    fn run(args: Vec<String>) -> Result<(), String> {
        let wanted = args.first().cloned();
        let scan_ms: u32 = args
            .get(1)
            .and_then(|s| s.parse().ok())
            .unwrap_or(8000);
        let hold_ms: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(0);
        println!(
            "scanning {scan_ms} ms for {:?}",
            wanted.as_deref().unwrap_or("<colmi|ring>")
        );
        let devices = ble::scan(scan_ms).map_err(ble_err)?;
        println!("scan: {} device(s)", devices.len());

        let target = devices
            .iter()
            .find(|d| match (&wanted, &d.name, &d.ring_id) {
                (Some(w), name, ring_id) => {
                    d.handle.to_string().eq_ignore_ascii_case(w)
                        || ring_id
                            .as_deref()
                            .is_some_and(|id| id.eq_ignore_ascii_case(w))
                        || name
                            .as_deref()
                            .is_some_and(|n| n.to_lowercase().contains(&w.to_lowercase()))
                }
                (None, Some(n), _) => {
                    let n = n.to_lowercase();
                    n.contains("colmi") || n.contains("ring")
                }
                _ => false,
            })
            .ok_or_else(|| format!("no matching ring among {} device(s)", devices.len()))?;

        println!(
            "target: {:?} handle={} rssi={:?}",
            target.name, target.handle, target.rssi
        );
        ble::connect(target.handle).map_err(ble_err)?;

        // Bind first: the ring is a vendor-channel device and treats an idle
        // link as stale, so we write before doing anything read-shaped.
        println!("-> subscribe {V1_NOTIFY}");
        ble::subscribe(V1_NOTIFY).map_err(ble_err)?;
        let bind = frame(0x10, &[0x01]);
        println!("-> bind 0x10 {}", hex(&bind));
        ble::write(V1_WRITE, &bind).map_err(ble_err)?;
        drain("bind", 2000);

        // DIS is deliberately not read here: CoreBluetooth stalls on this
        // ring's DIS service and the values are known (hardware RT12CR2_V1.0,
        // firmware RT12CR2_1.00.02_260929, empty serial).
        let battery = frame(0x03, &[]);
        println!("-> battery 0x03 {}", hex(&battery));
        ble::write(V1_WRITE, &battery).map_err(ble_err)?;
        drain("battery", 4000);

        if hold_ms > 0 {
            println!("--- holding link {hold_ms} ms: perform gestures now ---");
            let seen = drain("hold", hold_ms);
            println!("frames during hold: {seen}");
        }

        ble::disconnect().map_err(ble_err)?;
        Ok(())
    }
}

/// 16-byte primary-channel frame: `[cmd][14 payload][checksum]`.
fn frame(cmd: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; 16];
    out[0] = cmd;
    for (i, b) in payload.iter().take(14).enumerate() {
        out[1 + i] = *b;
    }
    out[15] = out[..15].iter().fold(0u8, |a, b| a.wrapping_add(*b));
    out
}

/// Drain notifications for `timeout_ms`, reporting decoded frames.
fn drain(label: &str, timeout_ms: u64) -> usize {
    let mut n = 0;
    let slices = 40u64; // 40 * slice, each bounded by the host to 1 s
    let slice = ((timeout_ms / slices).max(50)).min(1000) as u32;
    for _ in 0..slices {
        match ble::next_event(slice) {
            Ok(Some(Event::Notification(notification))) => {
                n += 1;
                match decode(&notification.data) {
                    Some(f) => println!(
                        "  {label} cmd=0x{:02x} err={} csum={} payload={}",
                        f.cmd,
                        f.error,
                        if f.checksum_ok { "ok" } else { "BAD" },
                        hex(f.payload)
                    ),
                    None => println!("  {label} raw={}", hex(&notification.data)),
                }
            }
            Ok(Some(Event::Disconnected(reason))) => {
                println!("  {label} disconnected: {reason}");
                break;
            }
            Ok(None) => {}
            Err(e) => {
                println!("  {label} event error: {e:?}");
                break;
            }
        }
    }
    n
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
