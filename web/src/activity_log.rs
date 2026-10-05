// A persistent history of recent `AppContext::flash()` confirmations
// ("added to album", "renamed", ...) with timestamps — `ctx.status` only
// ever shows the latest one, and only for a few seconds; this is where it
// goes afterward. Sits at the bottom of the page (last child of `.app`,
// not a floating overlay — see style.css), collapsible, remembering its
// open/closed state across reloads via `AppContext::activity_log_open`.

use leptos::prelude::*;

use crate::browser;
use crate::context::use_app;

#[component]
pub fn ActivityLogPanel() -> impl IntoView {
    let ctx = use_app();

    view! {
        <div class="activity-log" class:activity-log-open=move || ctx.activity_log_open.get()>
            <button
                type="button"
                class="activity-log-header"
                aria-expanded=move || ctx.activity_log_open.get().to_string()
                on:click=move |_| ctx.toggle_activity_log()
            >
                <span class="activity-log-caret" aria-hidden="true">{move || if ctx.activity_log_open.get() { "▾" } else { "▸" }}</span>
                <span>"Activity"</span>
                <span class="activity-log-count">{move || ctx.activity_log.get().len()}</span>
            </button>
            <Show when=move || ctx.activity_log_open.get()>
                <ul class="activity-log-list">
                    <Show when=move || ctx.activity_log.get().is_empty()>
                        <li class="activity-log-empty">"Nothing yet."</li>
                    </Show>
                    <For each=move || ctx.activity_log.get() key=|e| format!("{}-{}", e.at_ms, e.message) let:entry>
                        <li>
                            <span class="activity-log-time">{browser::format_time(entry.at_ms)}</span>
                            <span class="activity-log-message">{entry.message}</span>
                        </li>
                    </For>
                </ul>
            </Show>
        </div>
    }
}
