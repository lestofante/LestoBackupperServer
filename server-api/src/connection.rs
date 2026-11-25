// One port for everything, always TLS (tls.rs). After the handshake the
// first bytes tell the two clients apart: a browser starts an HTTP request
// ("GET /…", for the web client's files or a WebSocket, ws.rs), the app
// starts a frame with its 4-byte big-endian length (tcp.rs), which can't
// spell an HTTP method: that would be a frame of over a gigabyte, far past
// the 16 MiB limit.

use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;
use tracing::Instrument;

use crate::dispatch::SharedState;
use crate::web::Replay;

const HTTP_METHODS: [&[u8; 4]; 5] = [b"GET ", b"HEAD", b"POST", b"PUT ", b"OPTI"];

pub async fn handle_connection(stream: TcpStream, state: SharedState) {
    let peer = stream.peer_addr().map_or_else(|_| "?".to_string(), |a| a.to_string());
    let Some(mut tls) = crate::tls::accept(stream).await else { return };
    let mut first = [0u8; 4];
    if tls.read_exact(&mut first).await.is_err() {
        return;
    }
    let replay = Replay::new(first.to_vec(), tls);
    if HTTP_METHODS.contains(&&first) {
        crate::ws::route(replay, state, peer).await;
    } else {
        crate::tcp::serve(replay, state).instrument(tracing::info_span!("app", %peer)).await;
    }
}
