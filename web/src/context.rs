use std::rc::Rc;

use leptos::prelude::*;
use leptos::reactive::owner::LocalStorage;

use crate::browser;
use crate::ws_client::WsClient;

pub const KEYBOARD_SHORTCUTS_KEY: &str = "lestobackupper-keyboard-shortcuts";
pub const ACTIVITY_LOG_OPEN_KEY: &str = "lestobackupper-activity-log-open";

/// One line in the activity log panel — a past `flash()` message plus when
/// it happened. `at_ms` is wall-clock (`browser::now_ms()`), formatted for
/// display via `browser::format_time`.
#[derive(Debug, Clone, PartialEq)]
pub struct LogEntry {
    pub message: String,
    pub at_ms: f64,
}

/// Newest-first activity log stays bounded — a personal photo app run for
/// months would otherwise grow this signal (and the DOM list rendering
/// it) forever for something that's only ever useful as recent history.
const MAX_LOG_ENTRIES: usize = 100;

/// Shared app state, provided once at the root and read via `use_context`
/// from any component — avoids threading a dozen signals through every
/// intermediate component's props. Which *page* is showing is no longer
/// part of this — that's `leptos_router`'s job now, driven by the URL.
#[derive(Clone, Copy)]
pub struct AppContext {
    pub client: RwSignal<Option<Rc<WsClient>>, LocalStorage>,
    pub token: RwSignal<Option<String>>,
    pub is_admin: RwSignal<bool>,
    pub status: RwSignal<String>,
    /// Gates every custom keyboard behavior this client adds beyond plain
    /// browser defaults: access-key mnemonics, grid arrow-navigation and
    /// multi-select, and the picture view's Prev/Next/autoplay arrow keys.
    /// Defaults on; a Settings toggle flips it off for anyone who'd rather
    /// the app stay out of the way of their own key habits.
    pub keyboard_shortcuts: RwSignal<bool>,
    /// Recent `flash()` messages, newest first — the persistent history
    /// behind the bottom activity-log panel, as opposed to `status`'s one
    /// evaporating line.
    pub activity_log: RwSignal<Vec<LogEntry>>,
    /// The activity-log panel's open/closed state — persisted, so it
    /// stays however you last left it across reloads, but defaults to
    /// open (per the design: on by default, not a hidden feature someone
    /// has to discover).
    pub activity_log_open: RwSignal<bool>,
}

const FLASH_MS: i32 = 3000;

impl AppContext {
    pub fn new() -> Self {
        let shortcuts_enabled = browser::local_storage_get(KEYBOARD_SHORTCUTS_KEY).as_deref() != Some("0");
        let log_open = browser::local_storage_get(ACTIVITY_LOG_OPEN_KEY).as_deref() != Some("0");
        Self {
            client: RwSignal::new_local(None),
            token: RwSignal::new(None),
            is_admin: RwSignal::new(false),
            status: RwSignal::new(String::new()),
            keyboard_shortcuts: RwSignal::new(shortcuts_enabled),
            activity_log: RwSignal::new(Vec::new()),
            activity_log_open: RwSignal::new(log_open),
        }
    }

    /// A transient confirmation ("added to album") rather than an error
    /// someone needs to actively read and dismiss — clears itself after a
    /// few seconds. Plain `status.set(...)` has no such expiry, and since
    /// `status` is global (shown in the nav bar, not scoped to any one
    /// page) a message set that way just sits there indefinitely, visible
    /// no matter where you navigate next, until something else happens to
    /// overwrite it. Only clears if nothing newer replaced this exact
    /// message in the meantime.
    ///
    /// Also appends to `activity_log` — the flash line itself fades, but
    /// the fact that it happened (and when) stays in the persistent
    /// bottom panel.
    pub fn flash(&self, message: impl Into<String>) {
        let message = message.into();
        self.status.set(message.clone());
        let status = self.status;
        let message_for_timeout = message.clone();
        browser::set_timeout(FLASH_MS, move || {
            if status.get_untracked() == message_for_timeout {
                status.set(String::new());
            }
        });

        let entry = LogEntry { message, at_ms: browser::now_ms() };
        self.activity_log.update(|log| {
            log.insert(0, entry);
            log.truncate(MAX_LOG_ENTRIES);
        });
    }

    pub fn toggle_activity_log(&self) {
        let open = !self.activity_log_open.get_untracked();
        self.activity_log_open.set(open);
        browser::local_storage_set(ACTIVITY_LOG_OPEN_KEY, if open { "1" } else { "0" });
    }
}

pub fn use_app() -> AppContext {
    use_context::<AppContext>().expect("AppContext must be provided by the root component")
}
