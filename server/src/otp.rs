use serde::Serialize;
use totp_rs::{Algorithm, Secret, TOTP};

use crate::error::{Result, ServerError};

/// A freshly generated, not-yet-persisted TOTP secret. Nothing is written
/// to `credential.otp_secret` until the caller proves they scanned it
/// correctly by submitting one valid code — see `confirm`.
#[derive(Debug, Serialize)]
pub struct Enrollment {
    pub secret_base32: String,
    /// `otpauth://...` — render as a QR code client-side; nothing here
    /// generates the image itself.
    pub otpauth_url: String,
}

pub fn begin_enrollment(username: &str) -> Result<Enrollment> {
    let secret = Secret::generate_secret();
    let totp = build(&secret, username)?;
    Ok(Enrollment {
        secret_base32: secret.to_encoded().to_string(),
        otpauth_url: totp.get_url(),
    })
}

/// True iff `code` is valid *right now* for `secret_base32`. Used both to
/// confirm enrollment and on every subsequent login.
pub fn verify(secret_base32: &str, username: &str, code: &str) -> Result<bool> {
    let totp = build(&Secret::Encoded(secret_base32.to_string()), username)?;
    totp.check_current(code).map_err(|e| ServerError::Otp(e.to_string()))
}

fn build(secret: &Secret, username: &str) -> Result<TOTP> {
    TOTP::new(
        Algorithm::SHA1, // the one every authenticator app actually supports
        6,
        1,  // +/-1 step of clock skew tolerance
        30, // seconds per step, the RFC 6238 default
        secret.to_bytes().map_err(|e| ServerError::Otp(e.to_string()))?,
        Some("EventLog".to_string()),
        username.to_string(),
    )
    .map_err(|e| ServerError::Otp(e.to_string()))
}
