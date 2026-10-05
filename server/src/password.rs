use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::Argon2;
use rand_core::OsRng;

use crate::error::{Result, ServerError};

/// One `password_hash` column downstream: Argon2id encodes salt, algorithm
/// and cost inside this string (server.md) — never store a separate salt
/// column.
///
/// Only called from commands. Salting reads system randomness, so this
/// (like clock reads) must never run inside `apply()`.
pub fn hash(password: &str) -> Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|e| ServerError::PasswordHash(e.to_string()))
}

pub fn verify(password: &str, stored_hash: &str) -> Result<bool> {
    let parsed = PasswordHash::new(stored_hash).map_err(|e| ServerError::PasswordHash(e.to_string()))?;
    Ok(Argon2::default().verify_password(password.as_bytes(), &parsed).is_ok())
}
