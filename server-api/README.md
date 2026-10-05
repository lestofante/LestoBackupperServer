# server-api (the `lestobackupper-server` program)

> **Heads up:** this is a heavily "vibe-coded" experiment. The design and just
> about all of the code came out of an extended back-and-forth with an AI
> assistant, not a from-scratch human implementation, and it hasn't run in
> production anywhere. Treat it as exploratory rather than something to
> depend on as-is.

The network-facing surface described in `../docs/server.md`'s *"one HTTP
service... one auth system, one port"* line — except the wire format here
is FlatBuffers (`../schema`), not HTTP/JSON, so "one port" is actually two:
raw TCP for native clients, WebSocket for the browser (which can't open a
raw socket at all — that's the whole reason the second transport exists).
Both speak the exact same protocol and dispatch through the exact same
code; only the framing differs.

This crate is deliberately async (`tokio`), unlike `../server` and
[chronicler](https://github.com/MauroMombelli/chronicler), which are both synchronous throughout. The reasoning: each
request is independent, this machine has multiple cores, and the
underlying `diesel`/`r2d2` connection pool is already designed for
concurrent checkout — async buys real concurrency here for cheap. The seam
between the two is `dispatch::blocking` (`src/dispatch.rs`), which runs
each `server` call on tokio's blocking thread pool via `spawn_blocking`
rather than pretending `server` is async.

## Layout

- `src/dispatch.rs` — `RequestBody -> Result<ResponseBody, ApiError>`. The
  transport-agnostic core: parses wire strings into `server`'s domain
  types (`UserId`, `ObjectId`, ...), calls the matching
  `server::commands`/`server::queries` function, and builds the wire
  response. Neither transport module knows anything about `server`'s
  types; this is the only place that does. Has no I/O of its own — see
  `tests/tcp_integration.rs`/`tests/ws_integration.rs`, which exercise it
  through real sockets without spawning the binary.
- `src/error.rs` — `ApiError`: wraps `server::ServerError` plus the
  wire-parsing failures that only exist at this boundary (a malformed
  ULID, an untagged hash) — a JSON version of this API would have had
  serde reject those for free; FlatBuffers strings are just strings, so
  parsing and validating them is this crate's job.
- `src/framing.rs` — FlatBuffers encode/decode, shared by both transports.
- `src/tcp.rs` — raw-TCP transport: `[4-byte BE length][flatbuffers
  bytes]` framing, for native clients (Android/iOS) that don't need
  WebSocket's browser-compatibility overhead.
- `src/ws.rs` — WebSocket transport (`tokio-tungstenite`): one binary WS
  message per request/response, for the WASM web client (`../web`).
- `src/main.rs` — binds both listeners and spawns a connection handler per
  accepted socket.

Both transports spawn each *request* onto its own task, not just each
connection — a `request_id` field correlates a response back to its
request precisely because responses on one connection aren't guaranteed
to come back in submission order. A single writer task per connection
drains a channel and owns the actual socket write, so concurrently
dispatched requests can't interleave their responses mid-frame.

## Running

```sh
cargo xtask web          # once, and after changing the web client: builds web/pkg
cargo run --release --bin lestobackupper-server -- /tmp/lesto.db
cargo run --release --bin lestobackupper-server -- /tmp/lesto.db /tmp/objects 0.0.0.0:9000 0.0.0.0:9001
#                                                  db path       objects dir  app (TCP)    web + WebSocket
```

Every argument after the db path is optional and positional. Both
listeners bind all interfaces (`0.0.0.0`) by default so phones on the LAN
can connect — and both are plaintext, so pass `127.0.0.1:...` addresses
explicitly to keep a dev instance local-only.

The web client is served on the WebSocket port (`src/web.rs`): open
`http://<server>:9001/`. A request asking for a WebSocket goes to the API,
any other is a file of the built client, from `LESTO_WEB_DIR` (default
`./web`, so running from the repository root just works).

Settings, all optional (the older `EVENTLOG_*` names are still accepted):

| Variable | Default | |
|---|---|---|
| `LESTO_WEB_DIR` | `./web` | the built web client |
| `LESTO_SCRUB_INTERVAL_DAYS` | 7 | re-check each stored file's integrity this often; 0 = never |
| `LESTO_SCRUB_MAX_MIBPS` | 50 | read speed cap for that check |
| `LESTO_UPLOAD_EXPIRY_DAYS` | 7 | delete unfinished uploads untouched this long |
| `LESTO_RECLAIM_AFTER_DAYS` | 30 | free files no library or album holds after this long; 0 = keep forever |

On the very first run against a given database, this creates a default
`admin`/`admin` account (see `server::commands::user::ensure_bootstrap_admin`
and the "Accounts" section below) — a warning is logged when that happens.
Every later run against the same database is a silent no-op.

## Auth

Every op requires a valid session token except `CreateUserRequest` and
`LoginRequest`, checked once in `dispatch` before the exhaustive match —
see `dispatch`'s doc comment. Token resolution, the login rate limiter,
and the in-memory session cache all live in `server::commands::session`;
this crate only supplies the token string it read off the wire.

### Accounts: pending until an admin approves them

A fresh `CreateUserRequest` doesn't produce a usable account — new
accounts start `Pending`, and `LoginRequest` refuses one
(`ErrorCode::AccountPending`) until an admin sends `ApproveUserRequest`
for it (`ErrorCode::AdminRequired` if the caller isn't one). Full details
and the reasoning are in `../server`'s README; this crate just carries the
two new wire ops (`ApproveUserRequest`, the two new `ErrorCode` variants)
and calls `ensure_bootstrap_admin` once at startup. See
`tests/tcp_integration.rs` for the whole register → admin-approve → login
flow driven end to end over a real socket.

## What's not here yet

Same list as `../server`'s README, plus: no TLS on either transport (both
are plaintext — fine for local development, not for anything reachable
over an untrusted network).
