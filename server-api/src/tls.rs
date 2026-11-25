// TLS, the only way in. The server makes itself a certificate on first start
// (self-signed, kept next to the database), or uses one given in
// LESTO_TLS_CERT / LESTO_TLS_KEY (PEM, e.g. from Let's Encrypt). Clients
// don't need a certificate authority: the app shows this certificate's
// SHA-256 fingerprint on first login and pins it, and the server logs the
// same fingerprint at startup so the two can be compared.
//
// TLS only: a connection that doesn't open with a TLS handshake is closed.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use sha2::{Digest, Sha256};
use tokio::net::TcpStream;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};
use tokio_rustls::rustls::ServerConfig;
use tokio_rustls::TlsAcceptor;

pub struct Tls {
    acceptor: TlsAcceptor,
    /// "AB:CD:…", SHA-256 of the certificate: what the app pins.
    pub fingerprint: String,
}

static TLS: OnceLock<Tls> = OnceLock::new();

/// Sets the server's certificate, once, before accepting connections.
pub fn configure(tls: Tls) {
    let _ = TLS.set(tls);
}

/// The configured certificate's fingerprint.
pub fn configured_fingerprint() -> Option<&'static str> {
    TLS.get().map(|t| t.fingerprint.as_str())
}

/// The TLS handshake on a new connection; None (closing it) for anything
/// else: plaintext, a failed handshake, or no certificate configured.
pub async fn accept(stream: TcpStream) -> Option<tokio_rustls::server::TlsStream<TcpStream>> {
    let Some(tls) = TLS.get() else {
        tracing::error!("no TLS certificate configured: connection refused");
        return None;
    };
    match tls.acceptor.accept(stream).await {
        Ok(s) => Some(s),
        Err(e) => {
            tracing::debug!(error = %e, "TLS handshake failed (or not TLS): connection closed");
            None
        }
    }
}

/// LESTO_TLS_CERT + LESTO_TLS_KEY, or the server's own certificate next to
/// the database (`<db>.tls-cert.pem`, `<db>.tls-key.pem`), made on first start.
pub fn from_env(db_path: &str) -> Result<Tls, String> {
    let (cert_path, key_path) = match (crate::config::env("TLS_CERT"), crate::config::env("TLS_KEY")) {
        (Some(c), Some(k)) => (PathBuf::from(c), PathBuf::from(k)),
        (None, None) => {
            let cert = PathBuf::from(format!("{db_path}.tls-cert.pem"));
            let key = PathBuf::from(format!("{db_path}.tls-key.pem"));
            if !cert.exists() || !key.exists() {
                create_self_signed(&cert, &key)?;
                tracing::info!(cert = %cert.display(), "made a TLS certificate for this server");
            }
            (cert, key)
        }
        _ => return Err("set both LESTO_TLS_CERT and LESTO_TLS_KEY, or neither".to_string()),
    };
    load(&cert_path, &key_path)
}

pub fn load(cert_path: &Path, key_path: &Path) -> Result<Tls, String> {
    let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut std::io::BufReader::new(
        std::fs::File::open(cert_path).map_err(|e| format!("cannot read {}: {e}", cert_path.display()))?,
    ))
    .collect::<Result<_, _>>()
    .map_err(|e| format!("bad certificate in {}: {e}", cert_path.display()))?;
    let key: PrivateKeyDer<'static> = rustls_pemfile::private_key(&mut std::io::BufReader::new(
        std::fs::File::open(key_path).map_err(|e| format!("cannot read {}: {e}", key_path.display()))?,
    ))
    .map_err(|e| format!("bad key in {}: {e}", key_path.display()))?
    .ok_or_else(|| format!("no private key in {}", key_path.display()))?;
    let leaf = certs.first().ok_or_else(|| format!("no certificate in {}", cert_path.display()))?;
    let fingerprint = fingerprint(leaf.as_ref());

    let provider = Arc::new(tokio_rustls::rustls::crypto::ring::default_provider());
    let config = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| e.to_string())?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| format!("certificate and key don't work together: {e}"))?;
    Ok(Tls { acceptor: TlsAcceptor::from(Arc::new(config)), fingerprint })
}

/// "AB:CD:…": SHA-256 of a DER certificate, as the app shows it.
pub fn fingerprint(der: &[u8]) -> String {
    Sha256::digest(der).iter().map(|b| format!("{b:02X}")).collect::<Vec<_>>().join(":")
}

fn create_self_signed(cert_path: &Path, key_path: &Path) -> Result<(), String> {
    let mut names = vec!["lestobackupper".to_string(), "localhost".to_string()];
    if let Ok(host) = std::fs::read_to_string("/etc/hostname") {
        let host = host.trim();
        if !host.is_empty() {
            names.push(host.to_string());
        }
    }
    let made = rcgen::generate_simple_self_signed(names).map_err(|e| format!("cannot make a certificate: {e}"))?;
    std::fs::write(cert_path, made.cert.pem()).map_err(|e| format!("cannot write {}: {e}", cert_path.display()))?;
    write_private(key_path, made.key_pair.serialize_pem().as_bytes())
}

/// The key is readable by the server's user only.
fn write_private(path: &Path, bytes: &[u8]) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .map_err(|e| format!("cannot write {}: {e}", path.display()))?;
        f.write_all(bytes).map_err(|e| format!("cannot write {}: {e}", path.display()))
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, bytes).map_err(|e| format!("cannot write {}: {e}", path.display()))
    }
}
