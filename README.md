# LestoBackupperServer

> **Heads up:** this is a heavily "vibe-coded" experiment. The design and just
> about all of the code came out of an extended back-and-forth with an AI
> assistant, not a from-scratch human implementation, and it hasn't run in
> production anywhere. Treat it as exploratory rather than something to
> depend on as-is.

Your own server for backing up photos and videos: the LestoBackupper
Android app uploads to it, and a web client browses it. Accounts, libraries, albums
shared with roles, resumable uploads, thumbnails, a periodic integrity check
of every stored file; all history is an event log
([chronicler](https://github.com/MauroMombelli/chronicler)) over SQLite.

| Directory | What |
|---|---|
| `schema/` | the wire protocol (FlatBuffers), shared by the server, the web client and the app |
| `server/` | the domain: accounts, files, albums, permissions, storage; no networking |
| `server-api/` | the `lestobackupper-server` program: TCP for the app, WebSocket + the web client for browsers |
| `web/` | the web client, Rust compiled to WebAssembly |
| `xtask/` | project tasks in Rust (`cargo xtask web`) |
| `docs/` | design notes |

## Run

```sh
rustup target add wasm32-unknown-unknown            # once
cargo xtask web                                     # build the web client
cargo run --release --bin lestobackupper-server -- data/lesto.db
```

The app connects to port 9000; open `http://<server>:9001/` in a browser.
The first run creates an `admin`/`admin` account: change its password.
Options and settings: `server-api/README.md`. Plain TCP for now (no TLS):
keep it on your LAN or behind a VPN.

Only Rust is needed (and `ffmpeg` for video thumbnails).

## Test

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

License: AGPL-3.0-or-later.
