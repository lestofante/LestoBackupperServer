// TLS only, on one port: a client pinning the server's certificate (as the
// app does) logs in; another certificate is refused; plaintext gets nothing;
// and a browser's HTTP request on the same port gets an HTTP answer.

use std::time::Duration;

use lestobackupper_schema::protocol::{LoginRequest, Request, RequestBody, Response, ResponseBody, ResponseRef};
use planus::ReadAsRoot;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;

mod common;

async fn send_login<S: AsyncWrite + Unpin>(stream: &mut S) -> std::io::Result<()> {
    let mut builder = planus::Builder::new();
    let body = RequestBody::LoginRequest(Box::new(LoginRequest { username: "admin".into(), password: "admin".into(), otp_code: None, device_label: "tls test".into() }));
    let bytes = builder.finish(&Request { request_id: 7, token: None, body }, None);
    stream.write_u32(bytes.len() as u32).await?;
    stream.write_all(bytes).await?;
    stream.flush().await
}

async fn read_response<S: AsyncRead + Unpin>(stream: &mut S) -> std::io::Result<Response> {
    let len = stream.read_u32().await?;
    let mut buf = vec![0u8; len as usize];
    stream.read_exact(&mut buf).await?;
    Ok(Response::try_from(ResponseRef::read_as_root(&buf).unwrap()).unwrap())
}

#[tokio::test]
async fn tls_only_one_port_for_the_app_and_browsers() {
    let (dir, addr) = common::spawn_server().await;
    assert!(dir.path().exists());

    // Pinned: the app logs in.
    let mut app = common::connect(addr).await;
    send_login(&mut app).await.unwrap();
    assert!(matches!(read_response(&mut app).await.unwrap().body, ResponseBody::LoginResponse(_)));

    // Another certificate expected: the handshake fails.
    assert!(common::connect_pinned(addr, "00:11").await.is_err());

    // Plaintext: closed, no answer.
    tokio::time::sleep(Duration::from_millis(1100)).await; // login rate limit
    let mut plain = TcpStream::connect(addr).await.unwrap();
    let _ = send_login(&mut plain).await;
    assert!(read_response(&mut plain).await.is_err(), "plaintext must get nothing");

    // A browser on the same port: HTTP over TLS (no web client configured here: a 404 page).
    let mut browser = common::connect(addr).await;
    browser.write_all(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n").await.unwrap();
    let mut answer = String::new();
    browser.read_to_string(&mut answer).await.ok();
    assert!(answer.starts_with("HTTP/1.1 404"), "got {answer:?}");
}

#[test]
fn the_certificate_is_made_once_and_kept() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("test.db");
    let first = server_api::tls::from_env(db.to_str().unwrap()).unwrap().fingerprint;
    assert!(dir.path().join("test.db.tls-cert.pem").is_file());
    assert_eq!(first, server_api::tls::from_env(db.to_str().unwrap()).unwrap().fingerprint);
}
