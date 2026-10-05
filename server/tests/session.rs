// The rate limiter (commands::session::LAST_LOGIN_ATTEMPT) is a single
// process-global static, so every test here that calls `login` must be
// serialized against the others — otherwise two tests' login calls could
// interleave and one would spuriously eat the other's rate-limit window.
// TEST_LOCK enforces that; each integration test *file* is its own
// process, so this doesn't need to (and can't) coordinate with other
// tests/*.rs files.

use std::sync::Mutex;
use std::time::Duration;

use server::commands::{session, user};
use server::queries;
use server::{EventLog, NodeId, ServerError, UserId};
use totp_rs::{Algorithm, Secret, TOTP};

static TEST_LOCK: Mutex<()> = Mutex::new(());

fn open() -> (tempfile::TempDir, EventLog<server::Server>) {
    let dir = tempfile::tempdir().unwrap();
    let log = EventLog::open(dir.path().join("test.db"), NodeId("n".to_string())).unwrap();
    (dir, log)
}

// Clears whatever the rate limiter remembers from a previous test in this
// file before starting a fresh one.
fn wait_out_rate_limit() {
    std::thread::sleep(Duration::from_millis(1100));
}

fn totp_for(secret_base32: &str, username: &str) -> TOTP {
    TOTP::new(Algorithm::SHA1, 6, 1, 30, Secret::Encoded(secret_base32.to_string()).to_bytes().unwrap(), Some("EventLog".to_string()), username.to_string()).unwrap()
}

// Must run first, on a fresh db, before any other `create_user` call — it's
// a no-op once `app_user` is non-empty (see `ensure_bootstrap_admin`'s doc
// comment), which is exactly what makes it safe to just always call this
// rather than tracking "have I already bootstrapped this db" separately.
fn bootstrap_admin(log: &EventLog<server::Server>) -> UserId {
    user::ensure_bootstrap_admin(log).unwrap().expect("fresh db: bootstrap admin should be created").actor
}

// New accounts start Pending (commands::user::create_user) and can't log
// in until an admin approves them — most of these tests aren't about that
// gate itself, so this collapses "register, then get approved" to one call.
fn create_active_user(log: &EventLog<server::Server>, admin: UserId, username: &str, password: &str) -> UserId {
    let target = user::create_user(log, username, password).unwrap().actor;
    user::approve_user(log, admin, target).unwrap();
    target
}

#[test]
fn login_then_authenticate_round_trip() {
    let _guard = TEST_LOCK.lock().unwrap();
    wait_out_rate_limit();
    let (_dir, log) = open();
    let admin = bootstrap_admin(&log);
    let alice = create_active_user(&log, admin, "alice", "hunter2");

    let token = session::login(&log, "alice", "hunter2", None, "laptop").unwrap().token;
    assert_eq!(session::authenticate(&log, &token).unwrap(), alice);
}

#[test]
fn login_rejects_wrong_password_and_unknown_username() {
    let _guard = TEST_LOCK.lock().unwrap();
    wait_out_rate_limit();
    let (_dir, log) = open();
    let admin = bootstrap_admin(&log);
    create_active_user(&log, admin, "alice", "hunter2");

    assert!(matches!(session::login(&log, "alice", "wrong", None, "laptop"), Err(ServerError::InvalidCredentials)));
    wait_out_rate_limit();
    assert!(matches!(session::login(&log, "nobody", "wrong", None, "laptop"), Err(ServerError::InvalidCredentials)));
}

#[test]
fn login_reports_is_admin_so_a_client_knows_whether_to_show_admin_ui() {
    let _guard = TEST_LOCK.lock().unwrap();
    wait_out_rate_limit();
    let (_dir, log) = open();
    let admin = bootstrap_admin(&log);
    create_active_user(&log, admin, "alice", "hunter2");

    let admin_login = session::login(&log, "admin", "admin", None, "laptop").unwrap();
    assert!(admin_login.is_admin);

    wait_out_rate_limit();
    let alice_login = session::login(&log, "alice", "hunter2", None, "laptop").unwrap();
    assert!(!alice_login.is_admin);
}

#[test]
fn change_password_requires_the_current_one_and_then_takes_effect() {
    let _guard = TEST_LOCK.lock().unwrap();
    wait_out_rate_limit();
    let (_dir, log) = open();
    let admin = bootstrap_admin(&log);
    let alice = create_active_user(&log, admin, "alice", "hunter2");

    assert!(matches!(user::change_password(&log, alice, "wrong-old-password", "newpass123"), Err(ServerError::InvalidCredentials)));

    user::change_password(&log, alice, "hunter2", "newpass123").unwrap();

    // The old password no longer works, the new one does.
    assert!(matches!(session::login(&log, "alice", "hunter2", None, "laptop"), Err(ServerError::InvalidCredentials)));
    wait_out_rate_limit();
    session::login(&log, "alice", "newpass123", None, "laptop").unwrap();
}

#[test]
fn list_pending_users_is_admin_only_and_excludes_active_accounts() {
    let _guard = TEST_LOCK.lock().unwrap();
    wait_out_rate_limit();
    let (_dir, log) = open();
    let admin = bootstrap_admin(&log);
    let alice = create_active_user(&log, admin, "alice", "hunter2"); // active — shouldn't show up
    let bob = user::create_user(&log, "bob", "pw").unwrap().actor; // still pending

    assert!(matches!(queries::list_pending_users(&log, alice), Err(ServerError::AdminRequired)));

    let pending = queries::list_pending_users(&log, admin).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].user_id, bob);
    assert_eq!(pending[0].username, "bob");
}

#[test]
fn bogus_token_is_rejected() {
    let (_dir, log) = open();
    assert!(matches!(session::authenticate(&log, "not-a-real-token"), Err(ServerError::InvalidToken)));
}

#[test]
fn second_login_within_a_second_is_rate_limited_globally() {
    let _guard = TEST_LOCK.lock().unwrap();
    wait_out_rate_limit();
    let (_dir, log) = open();

    // The gate runs before credential lookup, so even a doomed-to-fail
    // attempt still primes it — this isolates the rate limiter itself from
    // whether the credentials were ever going to be right.
    let _ = session::login(&log, "nobody", "wrong", None, "x");
    assert!(matches!(session::login(&log, "nobody", "wrong", None, "x"), Err(ServerError::RateLimited)));
}

#[test]
fn pending_account_cannot_log_in_until_approved() {
    let _guard = TEST_LOCK.lock().unwrap();
    wait_out_rate_limit();
    let (_dir, log) = open();
    let admin = bootstrap_admin(&log);
    let alice = user::create_user(&log, "alice", "hunter2").unwrap().actor;

    assert!(matches!(session::login(&log, "alice", "hunter2", None, "laptop"), Err(ServerError::AccountPending)));

    user::approve_user(&log, admin, alice).unwrap();
    wait_out_rate_limit();
    session::login(&log, "alice", "hunter2", None, "laptop").unwrap();
}

#[test]
fn only_an_admin_can_approve_a_pending_account() {
    let _guard = TEST_LOCK.lock().unwrap();
    wait_out_rate_limit();
    let (_dir, log) = open();
    let admin = bootstrap_admin(&log);
    let alice = create_active_user(&log, admin, "alice", "hunter2");
    let bob = user::create_user(&log, "bob", "swordfish").unwrap().actor;

    // Alice is an active, ordinary user — not an admin — so she can't
    // approve bob no matter how active her own account is.
    assert!(matches!(user::approve_user(&log, alice, bob), Err(ServerError::AdminRequired)));
}

#[test]
fn revoke_only_removes_the_owning_users_session() {
    let _guard = TEST_LOCK.lock().unwrap();
    wait_out_rate_limit();
    let (_dir, log) = open();
    let admin = bootstrap_admin(&log);
    let alice = create_active_user(&log, admin, "alice", "hunter2");
    let bob = create_active_user(&log, admin, "bob", "swordfish");

    let token = session::login(&log, "alice", "hunter2", None, "laptop").unwrap().token;

    // Bob can't revoke alice's session just by knowing the token string.
    session::revoke(&log, bob, &token).unwrap();
    assert_eq!(session::authenticate(&log, &token).unwrap(), alice);

    session::revoke(&log, alice, &token).unwrap();
    assert!(matches!(session::authenticate(&log, &token), Err(ServerError::InvalidToken)));
}

#[test]
fn list_sessions_reports_device_labels() {
    let _guard = TEST_LOCK.lock().unwrap();
    wait_out_rate_limit();
    let (_dir, log) = open();
    let admin = bootstrap_admin(&log);
    let alice = create_active_user(&log, admin, "alice", "hunter2");

    session::login(&log, "alice", "hunter2", None, "laptop").unwrap();

    let sessions = session::list_sessions(&log, alice).unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].device_label, "laptop");
}

#[test]
fn otp_enrollment_requires_a_valid_code_and_then_gates_login() {
    let _guard = TEST_LOCK.lock().unwrap();
    wait_out_rate_limit();
    let (_dir, log) = open();
    let admin = bootstrap_admin(&log);
    let alice = create_active_user(&log, admin, "alice", "hunter2");

    // A wrong code can't confirm enrollment.
    let enrollment = user::begin_otp_enrollment(&log, alice).unwrap();
    assert!(matches!(user::confirm_otp_enrollment(&log, alice, &enrollment.secret_base32, "000000"), Err(ServerError::InvalidOtp)));

    // A right one does.
    let totp = totp_for(&enrollment.secret_base32, "alice");
    user::confirm_otp_enrollment(&log, alice, &enrollment.secret_base32, &totp.generate_current().unwrap()).unwrap();

    // Now the password alone isn't enough.
    wait_out_rate_limit();
    assert!(matches!(session::login(&log, "alice", "hunter2", None, "laptop"), Err(ServerError::OtpRequired)));

    // The right code completes it.
    wait_out_rate_limit();
    let token = session::login(&log, "alice", "hunter2", Some(&totp.generate_current().unwrap()), "laptop").unwrap().token;
    assert_eq!(session::authenticate(&log, &token).unwrap(), alice);

    // disable_otp turns the requirement back off.
    user::disable_otp(&log, alice).unwrap();
    wait_out_rate_limit();
    session::login(&log, "alice", "hunter2", None, "laptop").unwrap();
}
