// Raw-TCP transport for native clients (Android/iOS) that don't need
// WebSocket's browser-compatibility framing. One connection, many
// pipelined requests: each frame is `[4-byte BE length][flatbuffers
// bytes]`, and each request is dispatched on its own task so independent
// requests on the same connection actually run concurrently (not just
// interleaved) — responses carry `request_id` precisely because they
// aren't guaranteed to come back in submission order. The exception is a
// chunked upload's requests, which run in arrival order (lane.rs).

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tracing::Instrument;

use crate::dispatch::SharedState;
use crate::lane::{self, OrderedLane};
use crate::human::{Bytes, Rate};
use crate::{decode_request, dispatch, encode_error, encode_response};

const MAX_FRAME_BYTES: u32 = 16 * 1024 * 1024;

/// Below this, a frame's transfer time is mostly latency, not bandwidth,
/// so a per-request rate would be noise.
const RATE_MIN_BYTES: u32 = 64 * 1024;

/// Every log line from this connection carries its peer address (and, from
/// inside a request, that request's size and transfer rate) via spans.
pub async fn handle_connection(stream: TcpStream, state: SharedState) {
    let peer = stream.peer_addr().map_or_else(|_| "?".to_string(), |a| a.to_string());
    serve(stream, state).instrument(tracing::info_span!("tcp", %peer)).await
}

async fn serve(stream: TcpStream, state: SharedState) {
    tracing::info!("connected");
    let connected_at = Instant::now();
    let (mut requests, mut received, mut receiving, mut idle) = (0u64, 0u64, Duration::ZERO, Duration::ZERO);
    // Summed across request tasks, which run concurrently: can exceed wall time.
    let processing_nanos = Arc::new(AtomicU64::new(0));

    let (mut reader, mut writer) = stream.into_split();
    let (tx, mut rx) = mpsc::unbounded_channel::<Vec<u8>>();

    // One writer task drains the channel — the only thing allowed to touch
    // the write half, so concurrently-dispatched requests can't interleave
    // their responses mid-frame on the wire.
    let writer_task = tokio::spawn(async move {
        while let Some(frame) = rx.recv().await {
            if write_frame(&mut writer, &frame).await.is_err() {
                break;
            }
        }
    });

    // A chunked upload's requests, in arrival order (see lane.rs).
    let uploads = OrderedLane::new();

    loop {
        // Nothing arriving: the client is busy (reading/hashing its next
        // file) or waiting on our response before it sends the next one.
        let waiting_since = Instant::now();
        let Ok(len) = reader.read_u32().await else { break };
        idle += waiting_since.elapsed();

        if len > MAX_FRAME_BYTES {
            tracing::warn!(frame = %Bytes(len.into()), max = %Bytes(MAX_FRAME_BYTES.into()), "frame too large, dropping connection");
            break; // a client sending this is either broken or hostile
        }

        // Timed from the length prefix: the client writes the body right
        // behind it, so this is the transfer time of the body.
        let started = Instant::now();
        let mut buf = vec![0u8; len as usize];
        if reader.read_exact(&mut buf).await.is_err() {
            break;
        }
        let received_in = started.elapsed();
        requests += 1;
        received += u64::from(len);
        receiving += received_in;

        let span = tracing::info_span!("req", bytes = len, rate = tracing::field::Empty);
        if len >= RATE_MIN_BYTES {
            span.record("rate", tracing::field::display(Rate { bytes: len.into(), elapsed: received_in }));
        }

        let decoded = decode_request(&buf);
        let in_order = matches!(&decoded, Ok((_, _, body)) if lane::must_run_in_order(body));
        let state = state.clone();
        let tx = tx.clone();
        let processing_nanos = processing_nanos.clone();
        let job = async move {
            let started = Instant::now();
            let response_bytes = match decoded {
                Ok((request_id, token, body)) => match dispatch(&state, token.as_deref(), body).await {
                    Ok(response) => encode_response(request_id, response),
                    Err(err) => encode_error(request_id, &err),
                },
                // Couldn't even read request_id — nothing to correlate the
                // error to, so request_id 0 is the best we can do.
                Err(err) => encode_error(0, &err),
            };
            processing_nanos.fetch_add(u64::try_from(started.elapsed().as_nanos()).unwrap_or(u64::MAX), Ordering::Relaxed);
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
    // Network speed while bytes were actually arriving, as opposed to the
    // whole session, which also includes the client's own pauses.
    let rate = if received >= u64::from(RATE_MIN_BYTES) { Rate { bytes: received, elapsed: receiving }.to_string() } else { "-".to_string() };
    tracing::info!(
        requests,
        received = %Bytes(received),
        %rate,
        session = ?connected_at.elapsed(),
        // Where the session went: bytes arriving (`rate` above), the link
        // idle between requests, and our own work. A client that waits for
        // each response before sending the next has `idle` ≈ `processing` +
        // its own per-file work; idle far above processing means the client
        // is the bottleneck.
        receiving = ?receiving,
        idle = ?idle,
        processing = ?Duration::from_nanos(processing_nanos.load(Ordering::Relaxed)),
        "disconnected"
    );
}

async fn write_frame(writer: &mut (impl AsyncWriteExt + Unpin), frame: &[u8]) -> std::io::Result<()> {
    writer.write_u32(frame.len() as u32).await?;
    writer.write_all(frame).await?;
    writer.flush().await
}
