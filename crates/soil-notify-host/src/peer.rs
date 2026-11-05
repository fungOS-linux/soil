//! Unibus peer adapter for notify and ringer frames.
//!
//! The adapter is host-only. It receives an already-authenticated
//! `unibus/peer/1` session and routes only the notify/ringer kinds. A Spore
//! never sees the session, endpoint, token or peer identity.

use std::sync::Arc;

use anyhow::Context;
use serde_json::Value;
use unibus::{Envelope as WireEnvelope, Message, PeerSession};

use crate::{
    Envelope, NOTIFY_ACK, NOTIFY_PUBLISH, NOTIFY_STATE, NOTIFY_UPDATE, NOTIFY_WITHDRAW,
    NotifyBroker, NotifyError, RINGER_ACCEPT, RINGER_DECLINE, RINGER_END, RINGER_INCOMING, Ring,
    RingPhase,
};

pub struct NotifyPeerAdapter {
    broker: Arc<NotifyBroker>,
    installation_id: String,
    producer_id: String,
}

impl NotifyPeerAdapter {
    pub fn new(
        broker: Arc<NotifyBroker>,
        installation_id: impl Into<String>,
        producer_id: impl Into<String>,
    ) -> Self {
        Self {
            broker,
            installation_id: installation_id.into(),
            producer_id: producer_id.into(),
        }
    }

    /// Serve one authenticated peer session until the transport closes.
    pub async fn serve_session(&self, session: &mut dyn PeerSession) -> anyhow::Result<()> {
        loop {
            let (envelope, payload) = session
                .receive()
                .await
                .map_err(|error| anyhow::anyhow!("unibus receive: {error}"))?;
            if !payload.is_empty() {
                continue;
            }
            if self.handle(&envelope)? && envelope.message.kind != NOTIFY_ACK {
                let ack = WireEnvelope::new(
                    &self.installation_id,
                    &self.producer_id,
                    Some(&envelope.producer_id),
                    "",
                    Message::new(NOTIFY_ACK, serde_json::json!({"id": envelope.request_id})),
                );
                session
                    .send(&ack, &[])
                    .await
                    .map_err(|error| anyhow::anyhow!("unibus ack: {error}"))?;
            }
        }
    }

    /// True when there is at least one due outbox row.
    pub fn has_pending(&self, now_ms: u64) -> anyhow::Result<bool> {
        Ok(!self
            .broker
            .pending_outbox(now_ms, 1)
            .map_err(|error| anyhow::anyhow!("outbox: {error}"))?
            .is_empty())
    }

    /// Flush due outbox rows for this session. Rows addressed to `*` are
    /// broadcast; per-peer rows are the normal production fanout.
    pub async fn flush_session(
        &self,
        session: &mut dyn PeerSession,
        now_ms: u64,
        limit: u32,
    ) -> anyhow::Result<u32> {
        let rows = self
            .broker
            .pending_outbox(now_ms, limit)
            .map_err(|error| anyhow::anyhow!("outbox: {error}"))?;
        let mut sent = 0;
        for row in rows {
            if row.peer != "*" && row.peer != session.peer_id() {
                continue;
            }
            let body: Value = serde_json::from_str(&row.payload)
                .with_context(|| format!("outbox payload {}", row.id))?;
            let message = Message::new(&row.kind, body);
            let destination = (row.peer != "*").then_some(row.peer.as_str());
            let envelope = WireEnvelope::new(
                &self.installation_id,
                &self.producer_id,
                destination,
                "",
                message,
            );
            session
                .send(&envelope, &[])
                .await
                .map_err(|error| anyhow::anyhow!("unibus send: {error}"))?;
            // Iroh needs the admitted session to stay alive long enough for the
            // receiver to drain the uni stream. Acks are terminal and do not
            // need this grace period.
            if row.kind != NOTIFY_ACK {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
            self.broker
                .ack_outbox(row.id)
                .map_err(|error| anyhow::anyhow!("outbox ack: {error}"))?;
            sent += 1;
        }
        Ok(sent)
    }

    /// Handle one already-authenticated frame. Returns false for unrelated
    /// kinds, which a shared router can pass to another domain.
    pub fn handle(&self, wire: &WireEnvelope) -> anyhow::Result<bool> {
        let body = Value::Object(wire.message.body.clone());
        match wire.message.kind.as_str() {
            NOTIFY_PUBLISH | NOTIFY_UPDATE => {
                let envelope: Envelope =
                    serde_json::from_value(body).context("parse notify envelope")?;
                self.broker.apply_remote(envelope).map_err(notify_error)?;
                Ok(true)
            }
            NOTIFY_WITHDRAW => {
                let id = body
                    .get("id")
                    .and_then(Value::as_str)
                    .context("withdraw id")?;
                self.broker
                    .apply_remote_withdraw(id)
                    .map_err(notify_error)?;
                Ok(true)
            }
            NOTIFY_STATE => {
                let id = body.get("id").and_then(Value::as_str).context("state id")?;
                let read = body.get("read").and_then(Value::as_bool).unwrap_or(false);
                let dismissed = body
                    .get("dismissed")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                self.broker
                    .apply_remote_state(id, read, dismissed)
                    .map_err(notify_error)?;
                Ok(true)
            }
            NOTIFY_ACK => Ok(true),
            RINGER_INCOMING => {
                let ring: Ring = serde_json::from_value(body).context("parse ringer envelope")?;
                self.broker.apply_remote_ring(&ring).map_err(notify_error)?;
                Ok(true)
            }
            RINGER_ACCEPT | RINGER_DECLINE | RINGER_END => {
                let id = body
                    .get("id")
                    .and_then(Value::as_str)
                    .context("ringer id")?;
                let phase = match wire.message.kind.as_str() {
                    RINGER_ACCEPT => RingPhase::Accepted,
                    RINGER_DECLINE => RingPhase::Declined,
                    _ => RingPhase::Ended,
                };
                let answered_by = body.get("device").and_then(Value::as_str);
                self.broker
                    .apply_remote_ring_phase(id, phase, answered_by)
                    .map_err(notify_error)?;
                Ok(true)
            }
            _ => Ok(false),
        }
    }
}

fn notify_error(error: NotifyError) -> anyhow::Error {
    anyhow::anyhow!("{error}")
}

#[cfg(test)]
mod tests {
    use std::{collections::VecDeque, sync::Arc};

    use super::*;
    use crate::{Action, Priority, RingKind};
    use unibus::PeerFuture;

    struct FakeSession {
        peer: String,
        incoming: VecDeque<(WireEnvelope, Vec<u8>)>,
        sent: Vec<WireEnvelope>,
    }

    impl PeerSession for FakeSession {
        fn peer_id(&self) -> &str {
            &self.peer
        }

        fn send<'a>(
            &'a mut self,
            envelope: &'a WireEnvelope,
            _payload: &'a [u8],
        ) -> PeerFuture<'a, ()> {
            self.sent.push(envelope.clone());
            if envelope.message.kind != NOTIFY_ACK {
                let ack = WireEnvelope::new(
                    "home",
                    "studio",
                    Some("mac"),
                    "",
                    Message::new(NOTIFY_ACK, serde_json::json!({"id": envelope.request_id})),
                );
                self.incoming.push_back((ack, Vec::new()));
            }
            Box::pin(async { Ok(()) })
        }

        fn receive(&mut self) -> PeerFuture<'_, (WireEnvelope, Vec<u8>)> {
            let next = self.incoming.pop_front().expect("no incoming frame");
            Box::pin(async move { Ok(next) })
        }
    }

    fn envelope() -> Envelope {
        Envelope {
            id: "n1".into(),
            app_id: "dev.fungos.test".into(),
            source_device: "mac".into(),
            title: "Build failed".into(),
            body: "soil".into(),
            category: "build".into(),
            thread: None,
            group_key: None,
            created_at_ms: crate::now_ms(),
            expires_at_ms: None,
            priority: Priority::Active,
            sensitive: false,
            actions: vec![Action {
                id: "open".into(),
                label: "Open".into(),
                destructive: false,
            }],
            context: None,
        }
    }

    #[test]
    fn handles_remote_publish_with_dedupe() {
        let broker = Arc::new(NotifyBroker::in_memory("studio").unwrap());
        let adapter = NotifyPeerAdapter::new(Arc::clone(&broker), "home", "mac");
        let body = serde_json::to_value(envelope()).unwrap();
        let wire = WireEnvelope::new(
            "home",
            "mac",
            Some("studio"),
            "",
            Message::new(NOTIFY_PUBLISH, body.clone()),
        );
        assert!(adapter.handle(&wire).unwrap());
        assert!(adapter.handle(&wire).unwrap());
        assert_eq!(broker.inbox(&crate::Filter::default()).unwrap().len(), 1);
    }

    #[tokio::test]
    async fn flushes_a_publish_frame_on_a_peer_session() {
        let broker = Arc::new(NotifyBroker::in_memory("mac").unwrap());
        broker.publish(envelope()).unwrap();
        let adapter = NotifyPeerAdapter::new(Arc::clone(&broker), "home", "mac");
        let mut session = FakeSession {
            peer: "studio".into(),
            incoming: VecDeque::new(),
            sent: Vec::new(),
        };
        assert_eq!(
            adapter
                .flush_session(&mut session, crate::now_ms() + 1, 10)
                .await
                .unwrap(),
            1
        );
        assert_eq!(session.sent[0].message.kind, NOTIFY_PUBLISH);
        assert_eq!(session.sent[0].destination.as_deref(), None);
    }

    #[test]
    fn applies_one_ring_accept_claim() {
        let broker = Arc::new(NotifyBroker::in_memory("studio").unwrap());
        let adapter = NotifyPeerAdapter::new(Arc::clone(&broker), "home", "mac");
        let ring = Ring {
            id: "r1".into(),
            kind: RingKind::Call,
            source: "ada".into(),
            title: "Ada".into(),
            priority: Priority::Urgent,
            created_at_ms: crate::now_ms(),
            expires_at_ms: None,
            actions: Vec::new(),
        };
        let incoming = WireEnvelope::new(
            "home",
            "mac",
            Some("studio"),
            "",
            Message::new(RINGER_INCOMING, serde_json::to_value(&ring).unwrap()),
        );
        adapter.handle(&incoming).unwrap();
        let accept = WireEnvelope::new(
            "home",
            "mac",
            Some("studio"),
            "",
            Message::new(
                RINGER_ACCEPT,
                serde_json::json!({"id":"r1","device":"mac","phase":"accepted"}),
            ),
        );
        adapter.handle(&accept).unwrap();
        assert_eq!(
            broker.ring_state("r1").unwrap().answered_by.as_deref(),
            Some("mac")
        );
    }
}
