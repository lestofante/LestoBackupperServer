// An in-memory front for the `session` table, so a valid token doesn't
// cost a DB round-trip on every single API call — only the first time it's
// seen by this process. Capacity-bounded to a fixed memory budget, not a
// generic cache: this exists for exactly one purpose, and grows/shrinks
// only through the two operations that matter for that (a new session at
// login, an evicted one at revoke).
//
// Not a source of truth: the DB row is authoritative. This only ever
// *skips* a DB read on a cache hit; a miss still falls back to the DB (see
// commands::session::authenticate), which is what lets an existing session
// keep working after this process restarts with an empty cache.

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, OnceLock};

use crate::ids::UserId;

// Deliberately overestimated, not measured: a token hash held twice (once
// as the map key, once in `order` for eviction — both ~88-byte heap
// Strings for a 64-char hex digest), the cached UserId + expiry (~32
// bytes), and hash-table bucket/load-factor overhead (~48 bytes). Real
// entries are smaller; this just keeps the cap honest about the budget
// rather than promising more headroom than actually exists.
const ESTIMATED_BYTES_PER_ENTRY: usize = 256;
const MAX_CACHE_BYTES: usize = 100 * 1024 * 1024;
const MAX_ENTRIES: usize = MAX_CACHE_BYTES / ESTIMATED_BYTES_PER_ENTRY;

struct CachedSession {
    user_id: UserId,
    expires_at: Option<i64>,
}

#[derive(Default)]
struct Cache {
    entries: HashMap<String, CachedSession>,
    // Insertion order, for capacity-bounded FIFO eviction. Not a true LRU
    // — that would need an O(1) move-to-back on every hit, which isn't
    // worth the complexity here: login is already globally rate-limited to
    // one attempt/second (commands::session::rate_limit_login), capping
    // growth to at most ~86,400 new sessions a day — orders of magnitude
    // below MAX_ENTRIES even under sustained abuse.
    order: VecDeque<String>,
}

fn cache() -> &'static Mutex<Cache> {
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(Cache::default()))
}

pub(crate) fn get(token_hash: &str) -> Option<(UserId, Option<i64>)> {
    let cache = cache().lock().expect("session cache mutex poisoned");
    cache.entries.get(token_hash).map(|c| (c.user_id, c.expires_at))
}

pub(crate) fn put(token_hash: String, user_id: UserId, expires_at: Option<i64>) {
    let mut cache = cache().lock().expect("session cache mutex poisoned");
    if !cache.entries.contains_key(&token_hash) {
        if cache.order.len() >= MAX_ENTRIES {
            if let Some(oldest) = cache.order.pop_front() {
                cache.entries.remove(&oldest);
            }
        }
        cache.order.push_back(token_hash.clone());
    }
    cache.entries.insert(token_hash, CachedSession { user_id, expires_at });
}

/// Must run on revoke (and on finding a cached-but-expired entry) — a
/// stale hit here would mean a killed or expired session keeps
/// authenticating successfully from cache, defeating revocation entirely.
pub(crate) fn remove(token_hash: &str) {
    let mut cache = cache().lock().expect("session cache mutex poisoned");
    if cache.entries.remove(token_hash).is_some() {
        // Keeps `order.len() == entries.len()` exactly, so the capacity
        // check in `put` stays accurate over the cache's whole lifetime
        // instead of drifting from stale order entries after many
        // login/revoke cycles.
        if let Some(pos) = cache.order.iter().position(|k| k == token_hash) {
            cache.order.remove(pos);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn put_get_remove_round_trip() {
        let user_id = UserId::new();
        put("token-a".to_string(), user_id, None);
        assert_eq!(get("token-a"), Some((user_id, None)));

        remove("token-a");
        assert_eq!(get("token-a"), None);
    }

    #[test]
    fn miss_returns_none() {
        assert_eq!(get("never-inserted"), None);
    }
}
