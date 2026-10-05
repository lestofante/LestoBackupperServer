# server

> **Heads up:** this is a heavily "vibe-coded" experiment. The design and just
> about all of the code came out of an extended back-and-forth with an AI
> assistant, not a from-scratch human implementation, and it hasn't run in
> production anywhere. Treat it as exploratory rather than something to
> depend on as-is.

The domain application described in `../docs/server.md`, built on
[`chronicler`](https://github.com/MauroMombelli/chronicler) (`../docs/eventlog.md`). See `../docs/core.md` for
shared conventions.

This crate is the **domain layer only** — commands, queries, and the
`chronicler::Projection`. The network-facing surface (the actual wire
protocol both the web client and native apps talk) lives in
`../server-api`, on top of `../schema`'s FlatBuffers contract; see those
crates' READMEs for that half.

## Scope of this initial implementation

What's here: the event-sourced domain layer — users, content-addressed
files with dedup, and albums with ACL-style sharing and a trash bin — as a
`chronicler::Projection`, plus a command layer that does the
non-deterministic work (clock, password/OTP checks, name/hash lookups,
rate limiting) before handing `chronicler` a self-sufficient event to
append and apply; session tokens and OTP secrets bypass the event log
entirely (see below).

What's deliberately **not** here yet, per `../docs/server.md`:

- On-disk blob storage, `sendfile()` range requests, the write-blob-then-
  commit-DB ordering. `UploadFile` today only registers metadata (hash,
  size, mime) — no actual bytes move through this crate.
- Preview generation (`ffmpeg` frame extraction, downscaled proxies).
- The inotify-based Linux watcher.
- Server-to-server and mirror replication (`object_replica`, "send me
  events after seq N").

## Layout

- `src/action.rs` — the `Action` enum: one variant per domain fact.
- `src/domain/` — `apply()`: pure, deterministic writes against the tables
  in `migrations/`. No clock, no RNG, no lookups (eventlog.md).
- `src/commands/` — the non-deterministic half: resolves names and hashes,
  checks permissions, then calls `EventLog::append_and_apply`.
- `src/queries.rs` — read-only, permission-checked lookups. Nothing here
  appends an event.
- `src/token.rs` — session token generation/hashing (SHA-256; only the hash
  is ever stored).
- `src/session_cache.rs` — an in-memory, capacity-bounded (~100MB, by a
  conservative per-entry estimate, not exact byte accounting) cache in
  front of the `session` table, so a valid token skips the DB on every
  request after its first. Not a source of truth — a miss falls back to
  the DB, which is what lets existing sessions survive a process restart
  with an empty cache. Evicted on `revoke`, so a killed session can't keep
  authenticating from a stale hit.
- `src/otp.rs` — TOTP (RFC 6238) enrollment/verification, wrapping
  `totp-rs`.
- `src/projection.rs` — wires `domain`/`action` together as
  `chronicler::Projection for Server`.
- `src/ids.rs`, `src/object_id.rs` — ULID newtypes for `UserId`/`FileId`/
  `AlbumId`, and the `sha2-256:<hex>` tagged hash from `core.md`.

## Auth

- **Sessions and OTP secrets are not event-sourced.** `EventLog::connection()`
  is read-only by design; writing `session`/`credential.otp_secret` goes
  through `EventLog::writable_connection()` (a small addition to
  `chronicler` itself) and a plain `UPDATE`/`INSERT`, never
  `append_and_apply`. This is a hard requirement for OTP specifically: a
  password hash is one-way and safe to keep in an immutable, forever-
  replayable, `chronicler-dump`-able log; a TOTP secret is symmetric —
  anyone who ever reads it from history can generate valid codes forever,
  with no way to rotate it out of old events.
- **Tokens are stored hashed** (`token::hash`, SHA-256) — the raw token is
  returned to the client exactly once, at login.
- **Login is globally rate-limited**: one attempt per second, server-wide,
  checked before touching Argon2 or the database at all
  (`commands::session::LAST_LOGIN_ATTEMPT`). This is coarse — one client
  throttles everyone's login for that second — a deliberate simplification
  at this project's family scale, not a final design.
- **OTP** (`commands::user::begin_otp_enrollment` /
  `confirm_otp_enrollment`) is opt-in per account: a secret is generated
  and returned, but nothing is persisted until one valid code confirms it,
  so a botched QR scan can't lock the account out at the next login.

## Accounts: pending by default, admin-approved

`create_user` self-registration starts every new account `Pending`
(`app_user.status`) — `session::login` refuses a pending account
(`ServerError::AccountPending`) until an admin flips it to `Active` via
`commands::user::approve_user`. `is_admin` is a plain global flag on the
account (unrelated to any one album's ACL — approving accounts is a
system-wide concern, not an album-scoped one).

**Bootstrap**: `commands::user::ensure_bootstrap_admin`, called once at
`server-api` startup, is a no-op unless `app_user` is completely empty, in
which case it creates `admin`/`admin` — active immediately, `is_admin:
true`, no approval needed (there's no admin yet to grant one). Change that
password immediately; it's a deliberately guessable default for first
login, not a secret meant to stay in place.

Both `is_admin` and `status` ride on `Action::CreateUser` as fields with
serde defaults (`false` / `Active`) rather than a new action variant — a
historical event recorded before this feature existed correctly replays as
"not admin, already active", which is exactly what every account meant
before pending/approval existed. New events always set both explicitly.

## Example

```sh
cargo run --example gallery   # domain layer only, no network — see ../server-api for a live server
```

## Determinism

Like `chronicler` itself, `clippy.toml` bans `SystemTime::now` and
`rand::random` crate-wide; `src/time.rs` and `src/password.rs` are the
sanctioned, `#[allow]`-annotated escape hatches, called only from
commands. `tests/replay.rs` proves a representative command sequence
produces identical projection state whether run live or rebuilt from the
logged events alone — this deliberately excludes `session`/OTP state,
which isn't event-sourced and so isn't part of what replay reconstructs.
