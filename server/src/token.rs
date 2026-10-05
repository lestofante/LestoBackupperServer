use rand_core::{OsRng, RngCore};
use sha2::{Digest, Sha256};

/// A fresh, high-entropy session token, as hex. Handed to the client once,
/// at login — only its hash (see [`hash`]) is ever stored.
pub fn generate() -> String {
    let mut bytes = [0u8; 32];
    OsRng.fill_bytes(&mut bytes);
    hex(&bytes)
}

/// What actually goes in the `session.token` column — never the raw token,
/// so a database leak doesn't hand out live sessions (core.md's own choice
/// of SHA-256, reused here for the same "portable, hardware-accelerated"
/// reasons, not because a token needs the collision resistance of a
/// content hash).
pub fn hash(raw_token: &str) -> String {
    hex(&Sha256::digest(raw_token.as_bytes()))
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_is_deterministic_and_generate_is_not() {
        assert_eq!(hash("abc"), hash("abc"));
        assert_ne!(generate(), generate());
    }
}
