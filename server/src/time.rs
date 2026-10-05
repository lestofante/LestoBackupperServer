use std::time::{SystemTime, UNIX_EPOCH};

/// The only place this crate is allowed to read the clock — commands call
/// this, `apply()` never does (clippy.toml bans `SystemTime::now` crate-wide
/// so a stray call elsewhere fails the build instead of silently breaking
/// replay determinism).
#[allow(clippy::disallowed_methods)]
pub fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).expect("system clock before 1970").as_millis() as i64
}
