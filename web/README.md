# lestobackupper-web

> **Heads up:** this is a heavily "vibe-coded" experiment. The design and just
> about all of the code came out of an extended back-and-forth with an AI
> assistant, not a from-scratch human implementation, and it hasn't run in
> production anywhere. Treat it as exploratory rather than something to
> depend on as-is.

The web client described in `../docs/web.md`, written entirely in Rust and
compiled to WebAssembly — the deployed artifact is a handful of static
files (`index.html`, JS glue, a `.wasm` binary), which the server itself
serves (see "Building" below for the one real
requirement that comes with client-side routing). It talks to
`../server-api` over WebSocket, the only transport a browser sandbox
allows (see `../server-api`'s README for why raw TCP, which native clients
use, isn't an option here).

**Deliberate departure from `../docs/web.md`'s original design**: that spec
says the browser should never upload, staying preview-only, with ingest
staying device-only. This client uploads directly, because there's no
other client yet (no Linux/Android/iOS app built) — revisit once one
exists.

## UI: Leptos + leptos_router

Client-side rendering only (the `csr` feature) — no server-side rendering,
no server functions, no build-time backend. Routing (`leptos_router`) gives
every page — Library, an album, a single photo, Admin, Settings — its own
URL, so any of them can be bookmarked or shared directly, not just reached
by clicking through from Library.

## Building

```sh
rustup target add wasm32-unknown-unknown   # once
cargo xtask web                            # from the repository root
```

That builds the wasm and runs wasm-bindgen as a library (`../xtask`), so
there's nothing else to install: no `wasm-bindgen-cli`, no `trunk`, no
Node. It produces `pkg/lestobackupper_web.js` and
`pkg/lestobackupper_web_bg.wasm`, which `index.html` loads via a plain
`<script type="module">`.

`lestobackupper-server` serves this directory on its WebSocket port, so
open `http://<server>:9001/` and the client connects back to the same
address. Routing happens client-side (`/albums/<id>`, `/settings`, ...
aren't files): the server answers any path that isn't a file with
`index.html`, so bookmarks and reloads work. Behind another web server,
it needs the same fallback (nginx `try_files $uri /index.html;`).

## Layout

- `src/main.rs` — app shell: connects the `WsClient`, restores a session
  from `localStorage` if one's saved (re-validated against the server
  before being trusted — a stale/revoked token is dropped, not assumed
  good), and defines the routes. Auth gating doesn't care what the URL is:
  an unauthenticated visit to any path just shows the login form, and
  since the address bar keeps whatever path was requested, logging in
  lands exactly where that URL would've taken you anyway.
- `src/library.rs` — "All Photos" (server.md: `user_file` *is* the
  library, not an album row) — every file this account owns, regardless of
  which albums it's also filed under. This is where an upload always lands
  first; sorting it into albums after is optional. Upload via the button
  or by dragging files onto the page.
- `src/albums.rs` — the album list (each card shows up to 4 real photo
  thumbnails, not just a name) and the per-album detail view. Dropping
  files directly onto an album page uploads *and* files them into that
  album in one step, unlike Library's drop zone.
- `src/picture.rs` — a single photo at `/files/:id`. Deliberately doesn't
  know or care which list (Library, some album) it was reached from —
  server.md's file/album split means a file has no single canonical
  "home" to offer a back-link to; the browser's own back button already
  does that job correctly for whichever list you actually came from.
- `src/admin.rs` — the pending-account queue, admin-only (hidden from nav
  otherwise).
- `src/settings.rs` — dark mode, change password, OTP enrollment. The QR
  code is rendered entirely client-side (the `qrcode` crate, SVG output —
  no `image` crate, no raster codecs, nothing that would be awkward on
  wasm32) from the same `otpauth://` URL the manual-entry secret encodes;
  the server never sees or generates an image, only the secret.
- `src/api.rs` — typed wrappers around one `call()` primitive; each
  function builds exactly the `RequestBody` variant it needs and unwraps
  exactly the `ResponseBody` variant it expects.
- `src/ws_client.rs` — wraps `web_sys::WebSocket`'s callback-based API in
  an async `call()`. Supports genuinely concurrent requests, routed by
  `request_id` (server-api dispatches each request on its own task and
  doesn't guarantee responses come back in order) — this isn't optional:
  Library and an album view both fire off several concurrent
  `download_file` calls per page (one per photo thumbnail) on the same
  connection.
- `src/browser.rs` — small DOM-interop helpers (reading a picked/dropped
  `File`'s bytes, turning downloaded bytes into a `blob:` URL an `<img>`
  can load, `localStorage`, dark-mode attribute toggling) that don't
  belong to any one view.

## What's not here yet

No thumbnail/preview generation server-side (see `../server/README.md`) —
every image shown, at any size, is the full original's bytes; fine for a
personal collection, not something that scales to thousands of large
originals. No reject-a-pending-account or revoke-session UI. No album
rename or delete. No TLS on the WebSocket connection.
