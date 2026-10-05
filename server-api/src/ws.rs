// WebSocket transport — for the WASM web client, which can't open a raw
// TCP socket from inside a browser sandbox. One binary WS message per
// request/response; WebSocket already frames messages, so no manual
// length-prefixing here (unlike src/tcp.rs). Same per-request-task,
// single-writer-task shape as the TCP transport, for the same reason:
// independent requests on one connection shouldn't serialize behind each
// other.

use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tracing::Instrument;

use crate::dispatch::SharedState;
use crate::lane::{self, OrderedLane};
use crate::{decode_request, dispatch, encode_error, encode_response};

/// A connection to the web port: a WebSocket for the client's requests, or
/// a plain HTTP request for one of the client's files (web.rs).
pub async fn handle_connection(stream: TcpStream, state: SharedState) {
    let peer = stream.peer_addr().map_or_else(|_| "?".to_string(), |a| a.to_string());
    if !crate::web::wants_websocket(&stream).await {
        crate::web::serve_file(stream).instrument(tracing::debug_span!("http", %peer)).await;
        return;
    }
    serve(stream, state).instrument(tracing::info_span!("ws", %peer)).await
}

async fn serve(stream: TcpStream, state: SharedState) {
    let ws_stream = match tokio_tungstenite::accept_async(stream).await {
        Ok(ws) => ws,
        Err(_) => return, // not a valid WS handshake
    };
    let (mut write, mut read) = ws_stream.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();

    let writer_task = tokio::spawn(async move {
        while let Some(frame) = rx.recv().await {
            if write.send(Message::Binary(frame)).await.is_err() {
                break;
            }
        }
    });

    // A chunked upload's requests, in arrival order (see lane.rs).
    let uploads = OrderedLane::new();

    while let Some(msg) = read.next().await {
        let Ok(msg) = msg else { break };
        let bytes = match msg {
            Message::Binary(bytes) => bytes,
            Message::Close(_) => break,
            // Ping/Pong/Text/Frame: tungstenite answers pings itself; a
            // client sending text here is speaking the wrong protocol on
            // this endpoint, so just ignore rather than tear down.
            _ => continue,
        };

        let span = tracing::info_span!("req", bytes = bytes.len());
        let decoded = decode_request(&bytes);
        let in_order = matches!(&decoded, Ok((_, _, body)) if lane::must_run_in_order(body));
        let state = state.clone();
        let tx = tx.clone();
        let job = async move {
            let response_bytes = match decoded {
                Ok((request_id, token, body)) => match dispatch(&state, token.as_deref(), body).await {
                    Ok(response) => encode_response(request_id, response),
                    Err(err) => encode_error(request_id, &err),
                },
                Err(err) => encode_error(0, &err),
            };
            let _ = tx.send(response_bytes);
        }
        .instrument(span);
        if in_order {
            uploads.push(job);
        } else {
            tokio::spawn(job);
        }
    }

    uploads.finish().await;
    drop(tx);
    let _ = writer_task.await;
}
