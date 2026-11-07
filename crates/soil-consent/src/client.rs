//! Blocking Unix-socket client for the host consent broker.

use std::{
    os::unix::net::UnixStream,
    path::{Path, PathBuf},
    time::Duration,
};

use crate::{Decision, Request, wire};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("consent socket: {0}")]
    Socket(String),
    #[error("consent protocol: {0}")]
    Protocol(String),
    #[error("consent denied or timed out: {0}")]
    Denied(String),
}

pub struct Client {
    path: PathBuf,
}

impl Client {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Send one request and block for its decision. The popup enforces its own
    /// request timeout; the socket timeout is the request timeout plus a
    /// bounded grace period so the caller cannot hang forever.
    pub fn request(&self, request: &Request) -> Result<Decision, Error> {
        wire::validate_socket(&self.path).map_err(Error::Socket)?;
        let stream = UnixStream::connect(&self.path).map_err(|e| Error::Socket(e.to_string()))?;
        let timeout = Duration::from_millis(request.timeout_ms.saturating_add(2_000));
        stream
            .set_read_timeout(Some(timeout))
            .map_err(|e| Error::Socket(e.to_string()))?;
        stream
            .set_write_timeout(Some(timeout))
            .map_err(|e| Error::Socket(e.to_string()))?;
        if !wire::peer_is_self(&stream).map_err(Error::Socket)? {
            return Err(Error::Socket("consent socket peer is not this user".into()));
        }
        let encoded = serde_json::to_vec(request).map_err(|e| Error::Protocol(e.to_string()))?;
        use std::io::Write;
        let mut writer = &stream;
        writer
            .write_all(&encoded)
            .map_err(|e| Error::Socket(e.to_string()))?;
        writer
            .write_all(b"\n")
            .map_err(|e| Error::Socket(e.to_string()))?;
        writer.flush().map_err(|e| Error::Socket(e.to_string()))?;
        match wire::read_response(&stream).map_err(Error::Protocol)? {
            wire::Response::Decision { decision } => {
                if decision.request_id != request.id {
                    return Err(Error::Protocol("response request id mismatch".into()));
                }
                if !decision.allowed {
                    return Err(Error::Denied(
                        decision.reason.unwrap_or_else(|| "denied".into()),
                    ));
                }
                Ok(decision)
            }
            wire::Response::Error { error } => Err(Error::Denied(error)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Kind, Scope};
    use std::os::unix::{fs::PermissionsExt, net::UnixListener};

    /// A Unix socket path has a hard limit (104 bytes on macOS), and the default
    /// temp dir there is longer than that on its own. Bind under `/tmp` with a
    /// short name so these tests measure the permission and round-trip logic
    /// rather than `SUN_LEN`.
    fn short_socket() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::Builder::new()
            .prefix("sc")
            .tempdir_in("/tmp")
            .unwrap();
        let path = dir.path().join("s.sock");
        (dir, path)
    }

    fn request() -> Request {
        Request {
            id: "req-client".into(),
            kind: Kind::Outbound,
            requester: "spore:mail".into(),
            principal: "oidc:example#avery".into(),
            action: "mail.send".into(),
            payload_digest: "sha256:abc".into(),
            effects: vec![crate::Effect::External],
            scopes: vec![],
            resources: vec![],
            requested_scope: Scope::Once,
            preview: crate::Preview::None,
            timeout_ms: 1_000,
        }
    }

    #[test]
    fn rejects_broad_socket_permissions_and_accepts_private_ones() {
        let (dir, path) = short_socket();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let _listener = UnixListener::bind(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(crate::wire::validate_socket(&path).is_ok());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).unwrap();
        assert!(crate::wire::validate_socket(&path).is_err());
    }

    #[test]
    fn client_round_trips_a_decision_and_checks_request_id() {
        let (dir, path) = short_socket();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let listener = UnixListener::bind(&path).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let request = crate::wire::read_request(&stream).unwrap();
            assert!(crate::wire::peer_is_self(&stream).unwrap());
            crate::wire::write_response(
                stream,
                &crate::wire::Response::Decision {
                    decision: Decision {
                        request_id: request.id,
                        allowed: true,
                        scope: Scope::Once,
                        reason: None,
                        decided_by: "test".into(),
                        decided_at_ms: 1,
                    },
                },
            )
            .unwrap();
        });
        let decision = Client::new(&path).request(&request()).unwrap();
        assert!(decision.allowed);
        server.join().unwrap();

        let listener = UnixListener::bind(dir.path().join("other.sock")).unwrap();
        std::fs::set_permissions(
            dir.path().join("other.sock"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let _ = crate::wire::read_request(&stream).unwrap();
            crate::wire::write_response(
                stream,
                &crate::wire::Response::Decision {
                    decision: Decision {
                        request_id: "different".into(),
                        allowed: true,
                        scope: Scope::Once,
                        reason: None,
                        decided_by: "test".into(),
                        decided_at_ms: 1,
                    },
                },
            )
            .unwrap();
        });
        assert!(matches!(
            Client::new(dir.path().join("other.sock")).request(&request()),
            Err(Error::Protocol(_))
        ));
        server.join().unwrap();
    }
}
