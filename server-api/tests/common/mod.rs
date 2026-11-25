// Shared by the integration tests: the server is TLS only, so each test
// binary gives it a certificate once, and clients connect over TLS pinning
// that certificate by its fingerprint, as the app does.
#![allow(dead_code)] // each test file uses a different part

use std::net::SocketAddr;
use std::sync::{Arc, OnceLock};

use server::{EventLog, NodeId, Server};
use tokio::net::{TcpListener, TcpStream};
use tokio_rustls::client::TlsStream;
use tokio_rustls::rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use tokio_rustls::rustls::pki_types::{CertificateDer, ServerName, UnixTime};
use tokio_rustls::rustls::{ClientConfig, DigitallySignedStruct, Error, SignatureScheme};
use tokio_rustls::TlsConnector;

/// The server's certificate (made once per test binary), its fingerprint.
pub fn fingerprint() -> &'static str {
    static FINGERPRINT: OnceLock<String> = OnceLock::new();
    FINGERPRINT.get_or_init(|| {
        let dir = Box::leak(Box::new(tempfile::tempdir().unwrap()));
        let tls = server_api::tls::from_env(dir.path().join("certs").to_str().unwrap()).unwrap();
        let fingerprint = tls.fingerprint.clone();
        server_api::tls::configure(tls);
        fingerprint
    })
}

/// A server on a free local port, with a fresh database (and the bootstrap admin).
pub async fn spawn_server() -> (tempfile::TempDir, SocketAddr) {
    fingerprint();
    let dir = tempfile::tempdir().unwrap();
    let log = EventLog::<Server>::open(dir.path().join("test.db"), NodeId("n".to_string())).unwrap();
    server::commands::user::ensure_bootstrap_admin(&log).unwrap();
    let state = Arc::new(server_api::AppState { log, objects_dir: dir.path().join("objects") });
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else { continue };
            tokio::spawn(server_api::handle_connection(stream, state.clone()));
        }
    });
    (dir, addr)
}

pub type Conn = TlsStream<TcpStream>;

/// A TLS connection accepting only the certificate with this fingerprint.
pub async fn connect_pinned(addr: SocketAddr, pin: &str) -> std::io::Result<Conn> {
    let config = ClientConfig::builder_with_provider(Arc::new(tokio_rustls::rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .unwrap()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(Pinned(pin.to_string())))
        .with_no_client_auth();
    let tcp = TcpStream::connect(addr).await?;
    TlsConnector::from(Arc::new(config)).connect(ServerName::try_from("lestobackupper").unwrap(), tcp).await
}

/// A TLS connection to the test server, pinned to its certificate.
pub async fn connect(addr: SocketAddr) -> Conn {
    connect_pinned(addr, fingerprint()).await.unwrap()
}

#[derive(Debug)]
struct Pinned(String);

impl ServerCertVerifier for Pinned {
    fn verify_server_cert(&self, end: &CertificateDer<'_>, _: &[CertificateDer<'_>], _: &ServerName<'_>, _: &[u8], _: UnixTime) -> Result<ServerCertVerified, Error> {
        if server_api::tls::fingerprint(end.as_ref()) == self.0 {
            Ok(ServerCertVerified::assertion())
        } else {
            Err(Error::General("certificate changed".to_string()))
        }
    }
    fn verify_tls12_signature(&self, m: &[u8], c: &CertificateDer<'_>, d: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, Error> {
        tokio_rustls::rustls::crypto::verify_tls12_signature(m, c, d, &tokio_rustls::rustls::crypto::ring::default_provider().signature_verification_algorithms)
    }
    fn verify_tls13_signature(&self, m: &[u8], c: &CertificateDer<'_>, d: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, Error> {
        tokio_rustls::rustls::crypto::verify_tls13_signature(m, c, d, &tokio_rustls::rustls::crypto::ring::default_provider().signature_verification_algorithms)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        tokio_rustls::rustls::crypto::ring::default_provider().signature_verification_algorithms.supported_schemes()
    }
}
