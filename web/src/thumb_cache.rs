// An in-memory `(file_id, variant) -> blob: URL` cache, alive for the
// page's lifetime. WASM in a browser tab is single-threaded, so a plain
// `RefCell` behind a `thread_local!` is enough — no `Mutex` needed.
//
// This is what makes navigating Library -> an album -> back -> the same
// album not re-download and re-decode the same bytes over the wire every
// time: each view checks the cache before calling `api::download_file`,
// and only downloads on a genuine miss.

use std::cell::RefCell;
use std::collections::HashMap;

use lestobackupper_schema::protocol::ThumbnailVariant;

use crate::{api, browser};
use crate::ws_client::WsClient;

thread_local! {
    static CACHE: RefCell<HashMap<(String, u8), String>> = RefCell::new(HashMap::new());
}

fn key(file_id: &str, variant: ThumbnailVariant) -> (String, u8) {
    (file_id.to_string(), variant as u8)
}

pub fn get(file_id: &str, variant: ThumbnailVariant) -> Option<String> {
    CACHE.with(|c| c.borrow().get(&key(file_id, variant)).cloned())
}

pub fn insert(file_id: &str, variant: ThumbnailVariant, url: String) {
    CACHE.with(|c| c.borrow_mut().insert(key(file_id, variant), url));
}

/// Cache-through download: a hit never touches the network, a miss
/// downloads once and remembers the result for every later view (grid,
/// album preview, progressive-loading placeholder, ...) that asks for the
/// same file at the same variant.
pub async fn fetch(client: &WsClient, token: &str, file_id: &str, variant: ThumbnailVariant) -> Option<String> {
    if let Some(url) = get(file_id, variant) {
        return Some(url);
    }
    let (data, mime, _name) = api::download_file(client, token, file_id, variant).await.ok()?;
    let url = browser::object_url_for(&data, &mime).ok()?;
    insert(file_id, variant, url.clone());
    Some(url)
}

/// `Small` on a narrow (phone-width) viewport, `Medium` otherwise — the
/// "one for the phone where we need very little and one for pc" split.
/// Re-evaluated on every call rather than cached, since a real device
/// only ever reports one width anyway and this is cheap.
pub fn variant_for_grid() -> ThumbnailVariant {
    let width = web_sys::window().and_then(|w| w.inner_width().ok()).and_then(|v| v.as_f64()).unwrap_or(1024.0);
    if width < 700.0 {
        ThumbnailVariant::Small
    } else {
        ThumbnailVariant::Medium
    }
}
