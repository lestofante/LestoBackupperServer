// Small DOM-interop helpers that don't belong in any one view: reading a
// picked File as bytes, turning downloaded bytes into something an <img>
// can point at, and localStorage persistence.

use js_sys::{Array, Uint8Array};
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{Blob, BlobPropertyBag, Element, File};

/// Reads a picked `File`'s full contents. `File::array_buffer()` returns a
/// JS `Promise`; awaiting it is simpler than the older event-based
/// `FileReader` API for this one-shot use.
pub async fn read_file_bytes(file: &File) -> Result<Vec<u8>, JsValue> {
    let array_buffer = JsFuture::from(file.array_buffer()).await?;
    Ok(Uint8Array::new(&array_buffer).to_vec())
}

/// A `blob:` URL an `<img src>` can load directly — cheaper than a base64
/// data URI (no ~33% size inflation, no manual encoding) and exactly what
/// browsers expect for in-memory binary data. Never revoked: this is a
/// short-lived single-page app, not a long-running one where leaked blob
/// URLs would actually accumulate.
pub fn object_url_for(bytes: &[u8], mime: &str) -> Result<String, JsValue> {
    let array = Uint8Array::from(bytes);
    let parts = Array::new();
    parts.push(&array.buffer());
    let options = BlobPropertyBag::new();
    options.set_type(mime);
    let blob = Blob::new_with_u8_array_sequence_and_options(&parts, &options)?;
    web_sys::Url::create_object_url_with_blob(&blob)
}

/// The server's WebSocket: the same host and port this page was served
/// from (lestobackupper-server serves the web client on its WebSocket port),
/// `wss://` if the page came over https, since browsers block plaintext
/// WebSockets from secure pages. Falls back to 127.0.0.1:9001 without a
/// usable location (the page opened from a file).
pub fn server_ws_url() -> String {
    let location = web_sys::window().map(|w| w.location());
    let host = location
        .as_ref()
        .and_then(|l| l.host().ok()) // "host:port", IPv6 already bracketed
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| "127.0.0.1:9001".to_string());
    let scheme = match location.and_then(|l| l.protocol().ok()).as_deref() {
        Some("https:") => "wss",
        _ => "ws",
    };
    format!("{scheme}://{host}/")
}

pub fn local_storage_get(key: &str) -> Option<String> {
    web_sys::window()?.local_storage().ok()??.get_item(key).ok()?
}

pub fn local_storage_set(key: &str, value: &str) {
    if let Some(Ok(Some(storage))) = web_sys::window().map(|w| w.local_storage()) {
        let _ = storage.set_item(key, value);
    }
}

pub fn local_storage_remove(key: &str) {
    if let Some(Ok(Some(storage))) = web_sys::window().map(|w| w.local_storage()) {
        let _ = storage.remove_item(key);
    }
}

/// For a "< Back" control that goes wherever the browser's own back
/// button would — simpler and more correct than trying to reconstruct
/// "the list this photo was reached from" (there isn't always exactly
/// one: Library and any album containing the file all lead here).
pub fn go_back() {
    if let Some(window) = web_sys::window() {
        let _ = window.history().and_then(|h| h.back());
    }
}

/// One-shot timer for the picture view's autoplay slideshow: schedules
/// `f` to run once, `ms` from now, and returns the handle needed to cancel
/// it. `Closure::once_into_js` is the right tool for a one-shot callback —
/// it leaks the closure into JS ownership rather than requiring the caller
/// to keep a `Closure` alive for as long as the timer might fire.
pub fn set_timeout(ms: i32, f: impl FnOnce() + 'static) -> i32 {
    let Some(window) = web_sys::window() else { return 0 };
    let closure = Closure::once_into_js(f);
    window.set_timeout_with_callback_and_timeout_and_arguments_0(closure.as_ref().unchecked_ref(), ms).unwrap_or(0)
}

pub fn clear_timeout(handle: i32) {
    if let Some(window) = web_sys::window() {
        window.clear_timeout_with_handle(handle);
    }
}

/// True while the currently focused element would itself consume a
/// keystroke (typing text, moving a text cursor, adjusting a number
/// input) — every custom keyboard handler in this app (grid navigation,
/// picture-view prev/next) must check this first and back off, so arrow
/// keys inside e.g. the autoplay-seconds field still just move the cursor
/// or change the number, not hijack the page underneath it.
pub fn is_editable_target() -> bool {
    let Some(document) = web_sys::window().and_then(|w| w.document()) else { return false };
    let Some(active) = document.active_element() else { return false };
    if active.is_content_editable() {
        return true;
    }
    matches!(active.tag_name().to_ascii_uppercase().as_str(), "INPUT" | "TEXTAREA" | "SELECT")
}

trait ContentEditable {
    fn is_content_editable(&self) -> bool;
}
impl ContentEditable for Element {
    fn is_content_editable(&self) -> bool {
        self.get_attribute("contenteditable").is_some_and(|v| v != "false")
    }
}

/// How many direct children of the element matching `selector` sit in the
/// grid's first row — derived from actual layout (children sharing the
/// minimum `offsetTop`) rather than a hardcoded column count, since the
/// CSS is `repeat(auto-fill, ...)` and the real count depends on viewport
/// width. Used to make ArrowUp/ArrowDown move a full row instead of just
/// one tile, the way a real grid (not a flat list) should. Falls back to
/// 1 (behaving like ArrowLeft/ArrowRight) if there's nothing to measure.
pub fn items_per_row(selector: &str) -> usize {
    let Some(document) = web_sys::window().and_then(|w| w.document()) else { return 1 };
    let Ok(nodes) = document.query_selector_all(selector) else { return 1 };
    let mut tops = Vec::with_capacity(nodes.length() as usize);
    for i in 0..nodes.length() {
        if let Some(el) = nodes.get(i).and_then(|n| n.dyn_into::<web_sys::HtmlElement>().ok()) {
            tops.push(el.offset_top());
        }
    }
    let Some(&first_top) = tops.first() else { return 1 };
    tops.iter().filter(|&&t| t == first_top).count().max(1)
}

/// Toggles a `data-theme` attribute on `<html>` — Leptos (in CSR mode)
/// mounts into `<body>` and never sees the `<html>` element, so this is a
/// direct, one-off DOM call rather than something reactive Leptos markup
/// can express.
pub fn set_dark_mode(enabled: bool) {
    let Some(document) = web_sys::window().and_then(|w| w.document()) else { return };
    let Some(html) = document.document_element() else { return };
    let _ = if enabled { html.set_attribute("data-theme", "dark") } else { html.remove_attribute("data-theme") };
}

/// Milliseconds since the Unix epoch, per the JS `Date.now()` clock — used
/// to timestamp activity-log entries. Not a duration/monotonic clock, just
/// wall-clock time for a human-readable "HH:MM:SS".
pub fn now_ms() -> f64 {
    js_sys::Date::now()
}

/// `now_ms()`'s value formatted as a local "HH:MM:SS" — `js_sys::Date`
/// does its own locale/timezone-aware formatting, so this doesn't need to
/// hand-roll any of that.
pub fn format_time(ms: f64) -> String {
    let date = js_sys::Date::new(&JsValue::from_f64(ms));
    format!("{:02}:{:02}:{:02}", date.get_hours(), date.get_minutes(), date.get_seconds())
}
