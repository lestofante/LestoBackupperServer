// The web client's files, served on the same port as its WebSocket: a
// browser gets the app from the server it then talks to, so there's no
// separate web server to run (and no port to keep in sync). A request
// asking to upgrade to WebSocket goes to ws.rs; any other is a plain
// HTTP GET for a file of the built client (`cargo xtask web` → web/).
//
// Single-page app: only index.html, style.css and pkg/* are real files;
// /albums/<id>, /settings, ... are routes the client handles itself, so a
// path that isn't a file gets index.html (what nginx's try_files would do).

use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};

static WEB_DIR: OnceLock<Option<PathBuf>> = OnceLock::new();

/// Where the built web client is (a directory with index.html), or None to serve nothing.
pub fn configure(dir: Option<PathBuf>) {
    let _ = WEB_DIR.set(dir);
}

/// LESTO_WEB_DIR, else ./web when it holds a built client (running from the repository).
pub fn dir_from_env() -> Option<PathBuf> {
    let dir = crate::config::env("WEB_DIR").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("web"));
    dir.join("index.html").is_file().then_some(dir)
}

fn dir() -> Option<&'static Path> {
    WEB_DIR.get().and_then(|d| d.as_deref())
}

/// Requests bigger than this aren't a browser asking for a file.
const MAX_HEAD: usize = 16 * 1024;

/// Reads an HTTP request's head (up to the blank line). None if the
/// connection closes first or the head is too big to be a browser's.
pub async fn read_head<S: AsyncRead + Unpin>(stream: &mut S) -> Option<Vec<u8>> {
    let mut head = Vec::with_capacity(1024);
    let mut byte = [0u8; 1];
    while find(&head, b"\r\n\r\n").is_none() {
        match stream.read(&mut byte).await {
            Ok(1) if head.len() < MAX_HEAD => head.push(byte[0]),
            _ => return None,
        }
    }
    Some(head)
}

/// Whether the request asks to upgrade to a WebSocket.
pub fn is_websocket(head: &[u8]) -> bool {
    let text = String::from_utf8_lossy(head).to_ascii_lowercase();
    text.lines().any(|l| l.starts_with("upgrade:") && l.contains("websocket"))
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Answers one plain HTTP request, whose head was already read, with a file
/// of the web client, then closes.
pub async fn serve_file<S: AsyncWrite + Unpin>(mut stream: S, head: &[u8]) {
    let request = String::from_utf8_lossy(head);
    let mut parts = request.lines().next().unwrap_or("").split_whitespace();
    let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or("/"));
    let response = match method {
        "GET" | "HEAD" => respond(target, method == "HEAD").await,
        _ => plain(405, "Method Not Allowed", "only GET here"),
    };
    let _ = stream.write_all(&response).await;
    let _ = stream.shutdown().await;
}

/// A stream that first gives back bytes already read from it (the request
/// head looked at to route the connection), then reads on as usual: the
/// WebSocket handshake needs to see the whole request.
pub struct Replay<S> {
    head: Vec<u8>,
    at: usize,
    inner: S,
}

impl<S> Replay<S> {
    pub fn new(head: Vec<u8>, inner: S) -> Self {
        Replay { head, at: 0, inner }
    }
}

impl<S: AsyncRead + Unpin> AsyncRead for Replay<S> {
    fn poll_read(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &mut ReadBuf<'_>) -> Poll<std::io::Result<()>> {
        if self.at < self.head.len() {
            let n = buf.remaining().min(self.head.len() - self.at);
            let at = self.at;
            buf.put_slice(&self.head[at..at + n]);
            self.at += n;
            return Poll::Ready(Ok(()));
        }
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl<S: AsyncWrite + Unpin> AsyncWrite for Replay<S> {
    fn poll_write(mut self: Pin<&mut Self>, cx: &mut Context<'_>, buf: &[u8]) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

async fn respond(target: &str, head_only: bool) -> Vec<u8> {
    let Some(root) = dir() else {
        return plain(404, "Not Found", "this server has no web client configured (LESTO_WEB_DIR)");
    };
    let Some(relative) = safe_path(target) else {
        return plain(400, "Bad Request", "bad path");
    };
    let mut file = root.join(&relative);
    if relative.as_os_str().is_empty() || !file.is_file() {
        if relative.extension().is_some() && !relative.as_os_str().is_empty() {
            return plain(404, "Not Found", "no such file"); // a missing asset, not a route
        }
        file = root.join("index.html");
    }
    match tokio::fs::read(&file).await {
        Ok(body) => {
            let mut out = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: {}\r\nContent-Length: {}\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n",
                content_type(&file),
                body.len()
            )
            .into_bytes();
            if !head_only {
                out.extend_from_slice(&body);
            }
            out
        }
        Err(_) => plain(404, "Not Found", "the web client isn't built: run `cargo xtask web`"),
    }
}

/// The request's path as a relative path inside the web directory, or None
/// if it tries to leave it ("..") or isn't valid.
fn safe_path(target: &str) -> Option<PathBuf> {
    let path = target.split(['?', '#']).next().unwrap_or("");
    let decoded = percent_decode(path)?;
    let mut out = PathBuf::new();
    for component in Path::new(decoded.trim_start_matches('/')).components() {
        match component {
            Component::Normal(part) => out.push(part),
            Component::CurDir => {}
            _ => return None,
        }
    }
    Some(out)
}

fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = std::str::from_utf8(bytes.get(i + 1..i + 3)?).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    let s = String::from_utf8(out).ok()?;
    if s.contains('\0') || s.contains('\\') {
        return None;
    }
    Some(s)
}

fn content_type(file: &Path) -> &'static str {
    match file.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "wasm" => "application/wasm", // required for WebAssembly.instantiateStreaming
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        _ => "application/octet-stream",
    }
}

fn plain(code: u16, reason: &str, body: &str) -> Vec<u8> {
    format!("HTTP/1.1 {code} {reason}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_stay_inside_the_web_directory() {
        assert_eq!(safe_path("/"), Some(PathBuf::new()));
        assert_eq!(safe_path("/pkg/app.js?v=2"), Some(PathBuf::from("pkg/app.js")));
        assert_eq!(safe_path("/albums/01ABC"), Some(PathBuf::from("albums/01ABC")));
        assert_eq!(safe_path("/my%20file.png"), Some(PathBuf::from("my file.png")));
        assert_eq!(safe_path("/../etc/passwd"), None);
        assert_eq!(safe_path("/pkg/%2e%2e/%2e%2e/secret"), None);
        assert_eq!(safe_path("/a\\..\\b"), None, "no Windows-style escapes either");
        assert_eq!(safe_path("/%zz"), None);
    }
}
