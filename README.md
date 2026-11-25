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

The app connects to port 9000, and so does a browser: `https://<server>:9000/`.
The first run creates an `admin`/`admin` account: change its password.
Options and settings: `server-api/README.md`. Connections use TLS with a
certificate the server makes itself: the app asks you to trust its
fingerprint (logged at startup) on first login.

Only Rust is needed (and `ffmpeg` for video thumbnails).

## Deploying to a server

`deploy/deploy.sh <host>` builds here, uploads over ssh and restarts the
server there. `<host>` is anything ssh takes, a `~/.ssh/config` name too:

```sh
deploy/deploy.sh myserver
```

It builds for the server's CPU (asked over ssh: x86_64, ARM 64/32-bit,
RISC-V) as a static binary, so a slow server never compiles anything and
its Linux distribution doesn't matter. Cross-building needs, once, on this
machine (the script lists whatever is missing):

```sh
rustup target add aarch64-unknown-linux-musl   # the server's CPU: the script names it
rustup target add wasm32-unknown-unknown       # the web client
sudo pacman -S zig                             # C compiler/linker for any CPU (or ziglang.org)
cargo install --locked cargo-zigbuild
```

Everything goes in `~/lestobackupper` on the server: the program, `web/`,
and `data/` (database, stored photos, TLS certificate: back this one up).

### One-time setup on the server

As the user that will run it (no root needed, except for `enable-linger`):

```sh
# from this repository, on your machine:
ssh myserver 'mkdir -p ~/.config/systemd/user ~/lestobackupper/data'
scp deploy/lestobackupper.service myserver:.config/systemd/user/

# then on the server (ssh myserver):
systemctl --user daemon-reload
systemctl --user enable lestobackupper
sudo loginctl enable-linger $USER   # keep it running after logout, start it at boot
sudo pacman -S ffmpeg               # optional: video thumbnails (Debian: apt install ffmpeg)
```

Then `deploy/deploy.sh myserver` from here. It prints the certificate's
fingerprint: the app asks you to confirm it on first login. Port 9000 must
be reachable from your phone (firewall).

By hand, without the script: build with
`cargo zigbuild --release --target <target> --bin lestobackupper-server` and
`cargo xtask web`, copy `target/<target>/release/lestobackupper-server` to
`~/lestobackupper/` and `web/index.html`, `web/style.css`, `web/pkg/` to
`~/lestobackupper/web/`, then `systemctl --user restart lestobackupper`.
Logs: `journalctl --user -u lestobackupper -f`.

## Test

```sh
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
```

License: AGPL-3.0-or-later.
