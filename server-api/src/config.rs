// Settings from environment variables: `LESTO_<NAME>`, or the older
// `EVENTLOG_<NAME>` (from before the project was renamed), still accepted.

/// The value of `LESTO_<name>`, else `EVENTLOG_<name>`.
pub(crate) fn env(name: &str) -> Option<String> {
    if let Ok(v) = std::env::var(format!("LESTO_{name}")) {
        return Some(v);
    }
    let old = format!("EVENTLOG_{name}");
    let v = std::env::var(&old).ok()?;
    tracing::warn!(%old, new = %format!("LESTO_{name}"), "old variable name, still accepted: rename it");
    Some(v)
}

/// A number setting, `default` if unset or not a number (with a warning).
pub(crate) fn env_f64(name: &str, default: f64) -> f64 {
    match env(name) {
        None => default,
        Some(v) => v.trim().parse().unwrap_or_else(|_| {
            tracing::warn!(name = %format!("LESTO_{name}"), value = %v, default, "not a number, using the default");
            default
        }),
    }
}
