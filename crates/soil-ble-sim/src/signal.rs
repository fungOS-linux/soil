//! Executable fusion policy for simulated device signals.
//!
//! The simulator is where a bad day gets replayed, so the rules that protect
//! the consumer live here as code and tests: per-device step counts are never
//! summed, a guest's signals are not visible to the owner, stale is unknown
//! rather than zero, and raw audio never becomes a bus payload.

use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Freshness {
    Fresh,
    Stale,
    Expired,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Number(f64),
    Count(u64),
    Text(String),
    /// Content-addressed audio. The bytes live in Records/blob storage.
    BlobRef(String),
    /// A raw capture, used only to prove that it is rejected by the bus.
    RawAudio(Vec<u8>),
    Unknown,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Signal {
    pub signal_id: String,
    pub kind: String,
    pub source_device: String,
    pub wearer: Option<String>,
    pub observed_at: u64,
    pub freshness: Freshness,
    pub value: Value,
}

impl Signal {
    /// A stale or expired value is unknown, never zero. Callers that need a
    /// number must handle `None`; there is deliberately no `unwrap_or(0)`.
    pub fn numeric(&self) -> Option<f64> {
        if self.freshness != Freshness::Fresh {
            return None;
        }
        match self.value {
            Value::Number(value) => Some(value),
            Value::Count(value) => Some(value as f64),
            _ => None,
        }
    }

    pub fn count(&self) -> Option<u64> {
        if self.freshness != Freshness::Fresh {
            return None;
        }
        match self.value {
            Value::Count(value) => Some(value),
            _ => None,
        }
    }

    /// Wearer is an explicit identity, not a device owner. A ring worn by a
    /// guest is the guest's data, even when the owner's host saw the packet.
    pub fn visible_to(&self, wearer: &str) -> bool {
        self.wearer.as_deref() == Some(wearer)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BusPayload {
    Json(String),
    BlobRef(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BusError {
    /// Raw audio belongs in Records/blob storage, never on the live bus.
    RawAudio,
    /// An audio segment must carry a content-addressed reference.
    AudioNeedsBlobRef,
    /// An empty or unknown signal cannot be published.
    Empty,
}

/// Step counts stay partitioned by device. There is intentionally no total:
/// summing two devices double-counts the same walk.
pub fn steps_by_device(signals: &[Signal]) -> BTreeMap<String, u64> {
    let mut steps: BTreeMap<String, u64> = BTreeMap::new();
    for signal in signals {
        if signal.kind != "body.steps.v1" {
            continue;
        }
        if let Some(count) = signal.count() {
            steps
                .entry(signal.source_device.clone())
                .and_modify(|value| *value = (*value).max(count))
                .or_insert(count);
        }
    }
    steps
}

/// The only audio shape allowed on the bus is a reference. This function is
/// the single gate; producers should call it rather than inspecting variants
/// themselves.
pub fn bus_payload(signal: &Signal) -> Result<BusPayload, BusError> {
    if signal.signal_id.is_empty() || signal.kind.is_empty() {
        return Err(BusError::Empty);
    }
    match &signal.value {
        Value::RawAudio(_) => Err(BusError::RawAudio),
        Value::BlobRef(reference) => Ok(BusPayload::BlobRef(reference.clone())),
        Value::Unknown => Err(BusError::Empty),
        _ if signal.kind == "audio.segment.v1" => Err(BusError::AudioNeedsBlobRef),
        Value::Number(value) => Ok(BusPayload::Json(format!("{{\"value\":{value}}}"))),
        Value::Count(value) => Ok(BusPayload::Json(format!("{{\"value\":{value}}}"))),
        Value::Text(value) => Ok(BusPayload::Json(format!("{{\"value\":{value:?}}}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signal(
        device: &str,
        wearer: &str,
        kind: &str,
        value: Value,
        freshness: Freshness,
    ) -> Signal {
        Signal {
            signal_id: format!("{device}:{kind}:1"),
            kind: kind.to_string(),
            source_device: device.to_string(),
            wearer: Some(wearer.to_string()),
            observed_at: 1,
            freshness,
            value,
        }
    }

    #[test]
    fn steps_are_per_device_never_summed() {
        let signals = vec![
            signal(
                "watch",
                "owner",
                "body.steps.v1",
                Value::Count(1_000),
                Freshness::Fresh,
            ),
            signal(
                "ring",
                "owner",
                "body.steps.v1",
                Value::Count(2_000),
                Freshness::Fresh,
            ),
        ];
        let steps = steps_by_device(&signals);
        assert_eq!(steps.get("watch"), Some(&1_000));
        assert_eq!(steps.get("ring"), Some(&2_000));
        assert_eq!(steps.len(), 2);
        assert!(!steps.values().any(|value| *value == 3_000));
    }

    #[test]
    fn wearers_do_not_see_each_other() {
        let signals = vec![
            signal(
                "watch",
                "owner",
                "body.hr.v1",
                Value::Number(62.0),
                Freshness::Fresh,
            ),
            signal(
                "ring",
                "guest",
                "body.hr.v1",
                Value::Number(88.0),
                Freshness::Fresh,
            ),
        ];
        assert_eq!(signals.iter().filter(|s| s.visible_to("owner")).count(), 1);
        assert_eq!(signals.iter().filter(|s| s.visible_to("guest")).count(), 1);
        assert_eq!(
            signals
                .iter()
                .filter(|s| s.visible_to("someone-else"))
                .count(),
            0
        );
    }

    #[test]
    fn stale_is_unknown_not_zero() {
        let stale = signal(
            "watch",
            "owner",
            "body.hr.v1",
            Value::Number(72.0),
            Freshness::Stale,
        );
        assert_eq!(stale.numeric(), None);
        assert_ne!(stale.numeric(), Some(0.0));
        let expired = signal(
            "watch",
            "owner",
            "body.hr.v1",
            Value::Number(72.0),
            Freshness::Expired,
        );
        assert_eq!(expired.numeric(), None);
    }

    #[test]
    fn raw_audio_never_reaches_the_bus() {
        let raw = signal(
            "omi",
            "owner",
            "audio.segment.v1",
            Value::RawAudio(vec![1, 2, 3]),
            Freshness::Fresh,
        );
        assert_eq!(bus_payload(&raw), Err(BusError::RawAudio));

        let reference = signal(
            "omi",
            "owner",
            "audio.segment.v1",
            Value::BlobRef("blob:sha256:abc".to_string()),
            Freshness::Fresh,
        );
        assert_eq!(
            bus_payload(&reference),
            Ok(BusPayload::BlobRef("blob:sha256:abc".to_string()))
        );
    }

    #[test]
    fn audio_segment_without_a_reference_is_refused() {
        let numeric = signal(
            "omi",
            "owner",
            "audio.segment.v1",
            Value::Number(1.0),
            Freshness::Fresh,
        );
        assert_eq!(bus_payload(&numeric), Err(BusError::AudioNeedsBlobRef));
    }
}
