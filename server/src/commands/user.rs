use chronicler::{Event, EventLog};
use diesel::dsl::exists;
use diesel::prelude::*;

use crate::action::{AccountStatus, Action};
use crate::error::{Result, ServerError};
use crate::ids::UserId;
use crate::otp;
use crate::password;
use crate::projection::Server;
use crate::schema::{app_user, credential};
use crate::time::now_ms;

/// Self-registration: the new account is its own actor, and starts
/// `Pending` — an admin must `approve_user` it before `session::login`
/// will accept it. `apply()` is never handed a plaintext password —
/// hashing (which reads system randomness for the salt) happens here, in
/// the command, not in `apply()`.
pub fn create_user(log: &EventLog<Server>, username: &str, password: &str) -> Result<Event<UserId, Action>> {
    let taken: bool = {
        let mut conn = log.connection()?;
        diesel::select(exists(app_user::table.filter(app_user::username.eq(username)))).get_result(&mut conn)?
    };
    if taken {
        return Err(ServerError::UsernameTaken(username.to_string()));
    }

    let user_id = UserId::new();
    let password_hash = password::hash(password)?;

    Ok(log.append_and_apply(
        user_id,
        Action::CreateUser { user_id, username: username.to_string(), password_hash, is_admin: false, status: AccountStatus::Pending },
        now_ms(),
    )?)
}

/// Called once at server startup (see `server-api`'s `main`). A no-op
/// unless `app_user` is completely empty, in which case it creates a
/// well-known `admin`/`admin` account — active immediately, no approval
/// step, since there's no admin yet to grant one. Change that password
/// immediately; it's a deliberately guessable default for first login,
/// not a secret.
pub fn ensure_bootstrap_admin(log: &EventLog<Server>) -> Result<Option<Event<UserId, Action>>> {
    let has_any_user: bool = {
        let mut conn = log.connection()?;
        diesel::select(exists(app_user::table.select(app_user::user_id))).get_result(&mut conn)?
    };
    if has_any_user {
        return Ok(None);
    }

    let user_id = UserId::new();
    let password_hash = password::hash("admin")?;
    let event = log.append_and_apply(
        user_id,
        Action::CreateUser { user_id, username: "admin".to_string(), password_hash, is_admin: true, status: AccountStatus::Active },
        now_ms(),
    )?;
    Ok(Some(event))
}

/// Admin-only: activates a pending account. `actor` must already be an
/// active admin themselves — checked by an explicit query here rather than
/// `commands::album::require_role`'s pattern, since this isn't an
/// album-scoped permission at all (see the "admin scope" design note in
/// `../error.rs`'s `AdminRequired` variant).
pub fn approve_user(log: &EventLog<Server>, actor: UserId, target: UserId) -> Result<Event<UserId, Action>> {
    if !is_admin(log, actor)? {
        return Err(ServerError::AdminRequired);
    }

    Ok(log.append_and_apply(actor, Action::ApproveUser { user_id: target }, now_ms())?)
}

pub(crate) fn is_admin(log: &EventLog<Server>, user_id: UserId) -> Result<bool> {
    let mut conn = log.connection()?;
    Ok(app_user::table.find(user_id.to_string()).select(app_user::is_admin).first(&mut conn).optional()?.unwrap_or(false))
}

/// Requires the caller's *current* password, not just a valid session —
/// possessing a token doesn't prove you still know the password, and
/// requiring it here is the usual defense against a workstation left
/// logged in. Event-sourced like `CreateUser`'s hash: one-way, safe to
/// keep in the immutable log forever.
pub fn change_password(log: &EventLog<Server>, actor: UserId, old_password: &str, new_password: &str) -> Result<()> {
    let current_hash: String = {
        let mut conn = log.connection()?;
        credential::table.find(actor.to_string()).select(credential::password_hash).first(&mut conn)?
    };
    if !password::verify(old_password, &current_hash)? {
        return Err(ServerError::InvalidCredentials);
    }

    let new_hash = password::hash(new_password)?;
    log.append_and_apply(actor, Action::ChangePassword { user_id: actor, password_hash: new_hash }, now_ms())?;
    Ok(())
}

/// Not an event: a login attempt isn't domain history, and sessions (which
/// must be revocable and expire) live outside the append-only log. This
/// only checks the credential; issuing and storing a session token is the
/// caller's job.
pub fn verify_password(log: &EventLog<Server>, username: &str, password: &str) -> Result<Option<UserId>> {
    let row: Option<(String, String)> = {
        let mut conn = log.connection()?;
        app_user::table
            .inner_join(credential::table.on(credential::user_id.eq(app_user::user_id)))
            .filter(app_user::username.eq(username))
            .select((app_user::user_id, credential::password_hash))
            .first(&mut conn)
            .optional()?
    };

    let Some((user_id, password_hash)) = row else {
        return Ok(None);
    };

    if password::verify(password, &password_hash)? {
        Ok(Some(user_id.parse().expect("user_id column always holds a valid UserId")))
    } else {
        Ok(None)
    }
}

/// `(user_id, password_hash, status, is_admin, otp_secret)`.
pub(crate) type Credential = (UserId, String, AccountStatus, bool, Option<String>);

/// Used by `commands::session::login`, which additionally needs the account
/// status (pending accounts can't log in), whether it's an admin (reported
/// back to the client so it knows whether to show admin-only UI), and the
/// OTP secret (if any, to decide whether a code is required).
pub(crate) fn lookup_credential(log: &EventLog<Server>, username: &str) -> Result<Option<Credential>> {
    let row: Option<(String, String, String, bool, Option<String>)> = {
        let mut conn = log.connection()?;
        app_user::table
            .inner_join(credential::table.on(credential::user_id.eq(app_user::user_id)))
            .filter(app_user::username.eq(username))
            .select((app_user::user_id, credential::password_hash, app_user::status, app_user::is_admin, credential::otp_secret))
            .first(&mut conn)
            .optional()?
    };
    Ok(row.map(|(user_id, password_hash, status, is_admin, otp_secret)| {
        (
            user_id.parse().expect("user_id column always holds a valid UserId"),
            password_hash,
            AccountStatus::parse(&status).expect("status column always holds a valid AccountStatus"),
            is_admin,
            otp_secret,
        )
    }))
}

/// Step one of turning on 2FA: generates a secret and returns it (with a
/// scannable `otpauth://` URL) without persisting anything yet.
/// `confirm_otp_enrollment` is what actually saves it, once the caller
/// proves they captured it correctly.
pub fn begin_otp_enrollment(log: &EventLog<Server>, actor: UserId) -> Result<otp::Enrollment> {
    otp::begin_enrollment(&username_of(log, actor)?)
}

/// Persists the OTP secret directly via `writable_connection` — never
/// through the event log; see the migration's comment on `credential` for
/// why. Requires one valid code first, so a botched QR scan can't silently
/// lock the account out at the next login.
pub fn confirm_otp_enrollment(log: &EventLog<Server>, actor: UserId, secret_base32: &str, code: &str) -> Result<()> {
    let username = username_of(log, actor)?;
    if !otp::verify(secret_base32, &username, code)? {
        return Err(ServerError::InvalidOtp);
    }

    let mut conn = log.writable_connection()?;
    diesel::update(credential::table.find(actor.to_string())).set(credential::otp_secret.eq(secret_base32)).execute(&mut conn)?;
    Ok(())
}

pub fn disable_otp(log: &EventLog<Server>, actor: UserId) -> Result<()> {
    let mut conn = log.writable_connection()?;
    diesel::update(credential::table.find(actor.to_string())).set(credential::otp_secret.eq(None::<String>)).execute(&mut conn)?;
    Ok(())
}

/// For display and logs: events and the wire only ever carry the UserId.
pub fn username_of(log: &EventLog<Server>, user_id: UserId) -> Result<String> {
    let mut conn = log.connection()?;
    Ok(app_user::table.find(user_id.to_string()).select(app_user::username).first(&mut conn)?)
}
