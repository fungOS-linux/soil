use std::{sync::Arc, time::Duration};

use iroh::{Endpoint, endpoint::presets};
use rustls::pki_types::ServerName;
use soil_edge::{Backend, Edge, EdgeConfig, Routes};
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    net::{TcpListener, TcpStream},
    time::timeout,
};

fn client_hello(name: &str) -> Vec<u8> {
    let config = rustls::ClientConfig::builder()
        .with_root_certificates(rustls::RootCertStore::empty())
        .with_no_client_auth();
    let name = ServerName::try_from(name.to_string()).unwrap();
    let mut connection = rustls::ClientConnection::new(Arc::new(config), name).unwrap();
    let mut bytes = Vec::new();
    connection.write_tls(&mut bytes).unwrap();
    bytes
}

async fn start_edge(
    sni: &str,
    backend: std::net::SocketAddr,
) -> (
    std::net::SocketAddr,
    tokio::task::JoinHandle<anyhow::Result<()>>,
) {
    let mut routes = Routes::default();
    routes.insert(sni, Backend::Tcp(backend)).unwrap();
    let mut config = EdgeConfig::new(routes);
    config.handshake_timeout = Duration::from_secs(2);
    config.connect_timeout = Duration::from_secs(2);
    let edge = Arc::new(Edge::bind(config).await.unwrap());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(edge.serve(listener));
    (address, task)
}

#[tokio::test]
async fn forwards_the_exact_client_hello_and_pipes_bytes() {
    let hello = client_hello("confluence.test");
    let backend = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let backend_address = backend.local_addr().unwrap();
    let expected = hello.clone();
    let backend_task = tokio::spawn(async move {
        let (mut stream, _) = backend.accept().await.unwrap();
        let mut received = vec![0u8; expected.len()];
        stream.read_exact(&mut received).await.unwrap();
        assert_eq!(received, expected, "edge must not rewrite the ClientHello");
        stream.write_all(b"backend").await.unwrap();
        stream.shutdown().await.unwrap();
    });

    let (edge_address, edge_task) = start_edge("confluence.test", backend_address).await;
    let mut client = TcpStream::connect(edge_address).await.unwrap();
    client.write_all(&hello).await.unwrap();
    let mut response = Vec::new();
    timeout(Duration::from_secs(2), client.read_to_end(&mut response))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(response, b"backend");

    backend_task.await.unwrap();
    edge_task.abort();
}

#[tokio::test]
async fn forwards_the_same_bytes_over_iroh() {
    let alpn = b"confluence/tls/1";
    let server = Endpoint::builder(presets::Minimal)
        .relay_mode(iroh::RelayMode::Disabled)
        .bind_addr("127.0.0.1:0")
        .unwrap()
        .alpns(vec![alpn.to_vec()])
        .bind()
        .await
        .unwrap();
    let server_address = server
        .bound_sockets()
        .into_iter()
        .find(|address| address.ip().is_loopback())
        .expect("loopback Iroh address");
    let server_id = server.id();
    let hello = client_hello("confluence.test");
    let expected = hello.clone();
    let server_task = tokio::spawn(async move {
        let incoming = server.accept().await.unwrap();
        let connection = incoming.await.unwrap();
        let (mut send, mut recv) = connection.accept_bi().await.unwrap();
        let mut received = vec![0u8; expected.len()];
        recv.read_exact(&mut received).await.unwrap();
        assert_eq!(received, expected, "Iroh must carry the same ClientHello");
        send.write_all(b"backend").await.unwrap();
        send.finish().unwrap();
        let _ = timeout(Duration::from_secs(2), connection.closed()).await;
    });

    let mut routes = Routes::default();
    routes
        .insert(
            "confluence.test",
            Backend::Iroh {
                endpoint: server_id,
                addr: server_address,
            },
        )
        .unwrap();
    let mut config = EdgeConfig::new(routes);
    config.iroh_bind = "127.0.0.1:0".parse().unwrap();
    config.handshake_timeout = Duration::from_secs(2);
    config.connect_timeout = Duration::from_secs(5);
    let edge = Arc::new(Edge::bind(config).await.unwrap());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let edge_address = listener.local_addr().unwrap();
    let edge_task = tokio::spawn(edge.serve(listener));

    let mut client = TcpStream::connect(edge_address).await.unwrap();
    client.write_all(&hello).await.unwrap();
    let mut response = Vec::new();
    timeout(Duration::from_secs(5), client.read_to_end(&mut response))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(response, b"backend");

    server_task.await.unwrap();
    edge_task.abort();
}

#[tokio::test]
async fn closes_unknown_sni_before_contacting_a_backend() {
    let backend = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let backend_address = backend.local_addr().unwrap();
    let backend_task = tokio::spawn(async move {
        timeout(Duration::from_millis(500), backend.accept())
            .await
            .is_err()
    });

    let (edge_address, edge_task) = start_edge("confluence.test", backend_address).await;
    let mut client = TcpStream::connect(edge_address).await.unwrap();
    client
        .write_all(&client_hello("unknown.test"))
        .await
        .unwrap();
    let mut response = Vec::new();
    let read = timeout(Duration::from_secs(2), client.read_to_end(&mut response))
        .await
        .unwrap();
    assert!(read.is_ok());
    assert!(response.is_empty());

    assert!(
        backend_task.await.unwrap(),
        "unknown SNI must not reach a backend"
    );
    edge_task.abort();
}
