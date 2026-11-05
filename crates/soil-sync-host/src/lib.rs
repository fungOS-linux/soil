//! Shared Unibus Iroh peer host for records sync.
//!
//! The host owns the `unibus/peer/1` listener, session verifier and records
//! store. A Spore only sees `soil:records`; it never imports Unibus or Iroh.
//! Geode and future application kinds share the same listener and session
//! layer, with their own message kinds and grants.

use std::sync::Arc;

use anyhow::Context;
use fungos_records::{PeerEndpoint, PeerSyncOutcome, Store};
use tokio::sync::Mutex;
use unibus::{PeerCandidate, PeerSession, PeerTransport};
use unibus_iroh::transport::{IrohListener, IrohTransport};

/// One Iroh-backed records peer.
///
/// `listener` is constructed by the host from the peer identity, endpoint key,
/// relay policy and a verified `SessionConfig`. Production config comes from
/// Mycelium/fpl-auth; this crate does not mint permissive session assertions.
pub struct RecordPeerHost {
    listener: IrohListener,
    store: Arc<Mutex<Store>>,
    endpoint: PeerEndpoint,
}

impl RecordPeerHost {
    pub fn new(listener: IrohListener, store: Store, endpoint: PeerEndpoint) -> Self {
        Self {
            listener,
            store: Arc::new(Mutex::new(store)),
            endpoint,
        }
    }

    pub async fn run(self) -> anyhow::Result<()> {
        loop {
            let mut session = self
                .listener
                .accept()
                .await
                .map_err(|error| anyhow::anyhow!("Iroh accept: {error}"))?;
            let store = Arc::clone(&self.store);
            let endpoint = self.endpoint.clone();
            tokio::spawn(async move {
                if let Err(error) = serve_session(&mut session, store, endpoint).await {
                    eprintln!("soil-sync-host: session ended: {error:#}");
                }
            });
        }
    }
}

async fn serve_session(
    session: &mut dyn PeerSession,
    store: Arc<Mutex<Store>>,
    endpoint: PeerEndpoint,
) -> anyhow::Result<()> {
    loop {
        let (envelope, payload) = session
            .receive()
            .await
            .map_err(|error| anyhow::anyhow!("Iroh receive: {error}"))?;
        if !payload.is_empty() {
            continue;
        }
        if fungos_records::is_sync_kind(&envelope.message.kind) {
            let mut store = store.lock().await;
            fungos_records::handle_peer_frame(session, &mut store, &endpoint, &envelope, &payload)
                .await
                .context("handle records peer frame")?;
        }
        // Unknown kinds are ignored for forward compatibility. A host with
        // Geode/other handlers routes them to the corresponding handler first.
    }
}

/// Client helper: open one Iroh peer session and sync one records book.
pub async fn sync_records_over_iroh(
    transport: &dyn PeerTransport,
    candidate: &PeerCandidate,
    assertion: &str,
    store: &mut Store,
    book: &str,
    endpoint: &PeerEndpoint,
) -> anyhow::Result<PeerSyncOutcome> {
    let mut session = transport
        .connect(candidate, assertion)
        .await
        .map_err(|error| anyhow::anyhow!("Iroh connect: {error}"))?;
    fungos_records::sync_peer_session(&mut *session, store, book, endpoint)
        .await
        .context("records peer sync")
}

/// Convenience wrapper for a host that already built an `IrohTransport`.
pub async fn sync_records_with_transport(
    transport: &IrohTransport,
    candidate: &PeerCandidate,
    assertion: &str,
    store: &mut Store,
    book: &str,
    endpoint: &PeerEndpoint,
) -> anyhow::Result<PeerSyncOutcome> {
    sync_records_over_iroh(transport, candidate, assertion, store, book, endpoint).await
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeSet,
        sync::Arc,
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    use ed25519_dalek::SigningKey;
    use iroh::{Endpoint, endpoint::presets};
    use mycelium_peer_protocol::{AccessRecord, AccessStatement};
    use unibus::{
        DirectSessionClaims, DirectSessionError, DirectSessionVerifier, Envelope, IrohPeerEndpoint,
        Message, PeerAccessPolicy, PeerBinding,
    };
    use unibus_iroh::PEER_ALPN;
    use unibus_iroh::grant::{GrantVerifier, Permit, TrustDocument, permit_scope};
    use unibus_iroh::transport::{IrohListener, IrohTransport, SessionConfig};

    use super::*;

    struct FixtureVerifier;
    impl DirectSessionVerifier for FixtureVerifier {
        fn verify(
            &self,
            assertion: &str,
            _: u64,
        ) -> Result<DirectSessionClaims, DirectSessionError> {
            serde_json::from_str(assertion).map_err(|error| DirectSessionError(error.to_string()))
        }
    }

    fn now_s() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
    }

    async fn endpoint() -> Endpoint {
        Endpoint::builder(presets::Minimal)
            .relay_mode(iroh::RelayMode::Disabled)
            .bind_addr("127.0.0.1:0")
            .unwrap()
            .alpns(vec![PEER_ALPN.as_bytes().to_vec()])
            .bind()
            .await
            .unwrap()
    }

    fn config(producer: &str) -> SessionConfig {
        SessionConfig {
            verifier: Arc::new(FixtureVerifier),
            audience: "test".into(),
            installation_id: "test".into(),
            local_producer: producer.into(),
            allowed_relays: BTreeSet::new(),
            access_policy: None,
            endpoint_identity: None,
        }
    }

    fn grant_config(producer: &str, verifier: Arc<GrantVerifier>) -> SessionConfig {
        let verifier_dyn: Arc<dyn DirectSessionVerifier> = verifier.clone();
        let policy: Arc<dyn PeerAccessPolicy> = verifier;
        SessionConfig {
            verifier: verifier_dyn,
            audience: "test".into(),
            installation_id: "test".into(),
            local_producer: producer.into(),
            allowed_relays: BTreeSet::new(),
            access_policy: Some(policy),
            endpoint_identity: None,
        }
    }

    #[tokio::test]
    async fn records_sync_over_real_iroh_peer_session() {
        let a = endpoint().await;
        let b = endpoint().await;
        let binding = |producer: &str, endpoint: &Endpoint| PeerBinding {
            producer_id: producer.into(),
            key_binding: format!("iroh:{}", endpoint.id()),
            candidates: vec![],
        };
        let claims = DirectSessionClaims {
            schema_version: 1,
            iss: "fixture".into(),
            aud: "test".into(),
            jti: "records-sync-test".into(),
            installation_id: "test".into(),
            iat: now_s(),
            exp: now_s() + 60,
            initiator: binding("records.macbook", &a),
            responder: binding("records.studio", &b),
            kinds: vec![
                "records.sync.pull.v1".into(),
                "records.sync.ops.v1".into(),
                "records.sync.push.v1".into(),
                "records.sync.ack.v1".into(),
                "records.sync.error.v1".into(),
            ],
            max_payload_bytes: 1024 * 1024,
        };
        let assertion = serde_json::to_string(&claims).unwrap();
        let candidate = PeerCandidate {
            peer_id: "records.studio".into(),
            site: None,
            endpoints: vec![],
            iroh_endpoints: vec![IrohPeerEndpoint {
                endpoint_id: b.id().to_string(),
                direct_addresses: b.addr().ip_addrs().copied().collect(),
                relay_urls: vec![],
            }],
            expires_at_unix_s: now_s() + 60,
        };

        let dir = tempfile::tempdir().unwrap();
        let mut server = Store::open(
            &dir.path().join("studio"),
            &dir.path().join("studio-device"),
        )
        .unwrap();
        let mut client =
            Store::open(&dir.path().join("neo"), &dir.path().join("neo-device")).unwrap();
        server
            .put_record(
                "personal/avery/contacts",
                "ada",
                "fungos.contacts/contact",
                1,
                serde_json::json!({"display_name": "Ada Lovelace"}),
            )
            .unwrap();

        let listener = IrohListener::new(b.clone(), config("records.studio"));
        let host = RecordPeerHost::new(
            listener,
            server,
            PeerEndpoint {
                installation_id: "test".into(),
                producer_id: "records.studio".into(),
            },
        );
        let task = tokio::spawn(host.run());
        tokio::time::sleep(Duration::from_millis(50)).await;

        let transport = IrohTransport::new(a.clone(), config("records.macbook"));
        let outcome = sync_records_with_transport(
            &transport,
            &candidate,
            &assertion,
            &mut client,
            "personal/avery/contacts",
            &PeerEndpoint {
                installation_id: "test".into(),
                producer_id: "records.macbook".into(),
            },
        )
        .await
        .unwrap();
        assert_eq!(outcome.pulled, 1);
        assert_eq!(outcome.pushed, 0);
        assert_eq!(
            client
                .list("personal/avery/contacts", None, 10)
                .unwrap()
                .len(),
            1
        );

        task.abort();
        a.close().await;
        b.close().await;
    }

    #[tokio::test]
    async fn records_sync_over_mycelium_signed_grant() {
        let a = endpoint().await;
        let b = endpoint().await;
        let now = now_s();
        let permit = Permit {
            audience: "test".into(),
            installation: "test".into(),
            initiator: PeerBinding {
                producer_id: "records.macbook".into(),
                key_binding: format!("iroh:{}", a.id()),
                candidates: vec![],
            },
            responder: PeerBinding {
                producer_id: "records.studio".into(),
                key_binding: format!("iroh:{}", b.id()),
                candidates: vec![],
            },
            kinds: vec![
                "records.sync.pull.v1".into(),
                "records.sync.ops.v1".into(),
                "records.sync.push.v1".into(),
                "records.sync.ack.v1".into(),
                "records.sync.error.v1".into(),
            ],
            max_payload_bytes: 1024 * 1024,
        };
        let scope = permit_scope(&permit).unwrap();
        let signer = SigningKey::from_bytes(&[41; 32]);
        let grant = AccessRecord::sign(
            &signer,
            AccessStatement::Grant {
                grant_id: "records-test".into(),
                principal: "records-test".into(),
                serial: 1,
                roles: vec![],
                scopes: vec![scope],
                unix_users: vec![],
                ssh_public_keys: vec![],
                oidc_audiences: vec![],
                oidc_ssh_key_exchange: false,
                not_before: now - 60,
                not_after: now + 600,
            },
        )
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        let trust_path = dir.path().join("trust.json");
        std::fs::write(
            &trust_path,
            serde_json::to_vec(&TrustDocument {
                grant: grant.clone(),
                revocations: vec![],
                permit,
            })
            .unwrap(),
        )
        .unwrap();
        let verifier = Arc::new(
            GrantVerifier::new(trust_path, BTreeSet::from([grant.signer.clone()])).unwrap(),
        );
        let assertion = verifier.assertion(now).unwrap();

        let candidate = PeerCandidate {
            peer_id: "records.studio".into(),
            site: None,
            endpoints: vec![],
            iroh_endpoints: vec![IrohPeerEndpoint {
                endpoint_id: b.id().to_string(),
                direct_addresses: b.addr().ip_addrs().copied().collect(),
                relay_urls: vec![],
            }],
            expires_at_unix_s: now + 60,
        };
        let mut server = Store::open(
            &dir.path().join("studio"),
            &dir.path().join("studio-device"),
        )
        .unwrap();
        let mut client =
            Store::open(&dir.path().join("neo"), &dir.path().join("neo-device")).unwrap();
        server
            .put_record(
                "personal/avery/contacts",
                "grace",
                "fungos.contacts/contact",
                1,
                serde_json::json!({"display_name": "Grace Hopper"}),
            )
            .unwrap();

        let listener =
            IrohListener::new(b.clone(), grant_config("records.studio", verifier.clone()));
        let host = RecordPeerHost::new(
            listener,
            server,
            PeerEndpoint {
                installation_id: "test".into(),
                producer_id: "records.studio".into(),
            },
        );
        let task = tokio::spawn(host.run());
        tokio::time::sleep(Duration::from_millis(50)).await;

        let transport = IrohTransport::new(a.clone(), grant_config("records.macbook", verifier));
        let outcome = sync_records_with_transport(
            &transport,
            &candidate,
            &assertion,
            &mut client,
            "personal/avery/contacts",
            &PeerEndpoint {
                installation_id: "test".into(),
                producer_id: "records.macbook".into(),
            },
        )
        .await
        .unwrap();
        assert_eq!(outcome.pulled, 1);
        assert_eq!(
            client
                .list("personal/avery/contacts", None, 10)
                .unwrap()
                .len(),
            1
        );

        task.abort();
        a.close().await;
        b.close().await;
    }

    #[tokio::test]
    async fn unknown_kinds_do_not_mutate_records() {
        // The router ignores unknown application kinds. This guard keeps the
        // shared host forward-compatible with Geode/other handlers.
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path(), &dir.path().join("device")).unwrap();
        let envelope = Envelope::new(
            "test",
            "records.macbook",
            Some("records.studio"),
            "",
            Message::new("geode.sync.probe.v1", serde_json::json!({})),
        );
        assert!(!fungos_records::is_sync_kind(&envelope.message.kind));
        assert!(
            store
                .list("personal/avery/contacts", None, 10)
                .unwrap()
                .is_empty()
        );
    }
}
