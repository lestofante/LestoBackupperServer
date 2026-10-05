use std::sync::Mutex;
use std::time::{Duration, Instant};

use chronicler::EventLog;
use diesel::prelude::*;

use super::user::lookup_credential;
use crate::action::AccountStatus;
use crate::error::{Result, ServerError};
use crate::ids::UserId;
use crate::otp;
use crate::projection::Server;
use crate::schema::session;
use crate::session_cache;
use crate::time::now_ms;
use crate::token;

const MIN_LOGIN_INTERVAL: Duration = Duration::from_secs(1);

// Global, not per-account or per-IP: one login attempt anywhere on the
// server per second, full stop. Coarse on purpose — see commands/session.rs
// doc comment on `login` for the tradeoff. Checked (and updated) *before*
// touching Argon2 or the database at all, so a burst costs nothing but a
// mutex lock, not a hash verification.
static LAST_LOGIN_ATTEMPT: Mutex<Option<Instant>> = Mutex::new(None);

fn rate_limit_login() -> Result<()> {
    let mut last = LAST_LOGIN_ATTEMPT.lock().expect("rate limiter mutex poisoned");
    let now = Instant::now();
    if let Some(prev) = *last {
        if now.duration_since(prev) < MIN_LOGIN_INTERVAL {
            return Err(ServerError::RateLimited);
        }
    }
    *last = Some(now);
    Ok(())
}

#[derive(Debug, Clone)]
pub struct LoginResult {
    pub token: String,
    pub is_admin: bool,
}

/// The only network-facing op that doesn't require an existing token.
/// Global-rate-limited (see `rate_limit_login`); requires the account to
/// be `Active` (a `Pending` self-registration needs `approve_user` first);
/// and — if the account has enrolled OTP — requires a valid current code
/// in addition to the password. Returns the raw token exactly once; only
/// its hash is stored.
pub fn login(log: &EventLog<Server>, username: &str, password: &str, otp_code: Option<&str>, device_label: &str) -> Result<LoginResult> {
    rate_limit_login()?;

    let Some((user_id, password_hash, status, is_admin, otp_secret)) = lookup_credential(log, username)? else {
        return Err(ServerError::InvalidCredentials);
    };

    if !crate::password::verify(password, &password_hash)? {
        return Err(ServerError::InvalidCredentials);
    }

    if status != AccountStatus::Active {
        return Err(ServerError::AccountPending);
    }

    if let Some(secret) = otp_secret {
        let Some(code) = otp_code else {
            return Err(ServerError::OtpRequired);
        };
        if !otp::verify(&secret, username, code)? {
            return Err(ServerError::InvalidOtp);
        }
    }

    let raw_token = token::generate();
    let hashed = token::hash(&raw_token);
    let mut conn = log.writable_connection()?;
    diesel::insert_into(session::table)
        .values((
            session::token.eq(&hashed),
            session::user_id.eq(user_id.to_string()),
            session::issued_at.eq(now_ms()),
            session::expires_at.eq(None::<i64>),
            session::device_label.eq(device_label),
        ))
        .execute(&mut conn)?;

    // Warms the cache immediately, so the very next request on this token
    // doesn't pay for a DB round-trip we could have avoided.
    session_cache::put(hashed, user_id, None);

    Ok(LoginResult { token: raw_token, is_admin })
}

/// The one check every other network-facing op runs before doing anything
/// else. `Err(InvalidToken)` covers "never existed" and "expired" alike —
/// deliberately not distinguished, so a client can't use the difference to
/// probe for valid-but-expired tokens.
///
/// Checks the in-memory cache first (`session_cache`) — a hit skips the DB
/// entirely. A miss (first request for a token in this process's lifetime,
/// or after a restart) falls back to the DB and warms the cache for next
/// time. The DB row stays authoritative either way; this only ever saves a
/// read, never replaces the source of truth.
pub fn authenticate(log: &EventLog<Server>, raw_token: &str) -> Result<UserId> {
    let hashed = token::hash(raw_token);

    if let Some((user_id, expires_at)) = session_cache::get(&hashed) {
        if !expires_at.is_some_and(|exp| exp <= now_ms()) {
            return Ok(user_id);
        }
        // Cached but expired: don't trust it, and don't let it keep
        // occupying a slot — fall through to the DB, which will also say no.
        session_cache::remove(&hashed);
    }

    let mut conn = log.connection()?;
    let row: Option<(String, Option<i64>)> = session::table.find(&hashed).select((session::user_id, session::expires_at)).first(&mut conn).optional()?;

    let Some((user_id, expires_at)) = row else {
        return Err(ServerError::InvalidToken);
    };
    if expires_at.is_some_and(|exp| exp <= now_ms()) {
        return Err(ServerError::InvalidToken);
    }

    let user_id: UserId = user_id.parse().expect("user_id column always holds a valid UserId");
    session_cache::put(hashed, user_id, expires_at);
    Ok(user_id)
}

/// A lost device must be killable (server.md) — deletes one session by its
/// raw token, but only if it belongs to `actor`, so knowing a token string
/// (without it being yours) isn't enough to revoke someone else's session.
///
/// Evicting the cache only on an actual deletion (not unconditionally)
/// matters: otherwise passing someone else's token here — without owning
/// it — would silently evict *their* valid cached session as a side effect,
/// even though the DB delete itself correctly did nothing.
pub fn revoke(log: &EventLog<Server>, actor: UserId, raw_token: &str) -> Result<()> {
    let hashed = token::hash(raw_token);
    let mut conn = log.writable_connection()?;
    let deleted = diesel::delete(session::table.find(&hashed).filter(session::user_id.eq(actor.to_string()))).execute(&mut conn)?;
    if deleted > 0 {
        session_cache::remove(&hashed);
    }
    Ok(())
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionSummary {
    pub device_label: String,
    pub issued_at: i64,
}

/// For a "your devices" / "sign out this one" screen. Never exposes the
/// token itself — there'd be no legitimate reason for a client to see
/// another device's raw token, and this table only stores hashes anyway.
pub fn list_sessions(log: &EventLog<Server>, actor: UserId) -> Result<Vec<SessionSummary>> {
    let mut conn = log.connection()?;
    let rows: Vec<(String, i64)> = session::table
        .filter(session::user_id.eq(actor.to_string()))
        .select((session::device_label, session::issued_at))
        .load(&mut conn)?;
    Ok(rows.into_iter().map(|(device_label, issued_at)| SessionSummary { device_label, issued_at }).collect())
}
