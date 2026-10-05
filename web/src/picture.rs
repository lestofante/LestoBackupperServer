// A single photo, either standalone at /files/:file_id (reached from
// Library — server.md's file/album split means a file has no single
// canonical "home" outside an album, so this form has no prev/next) or
// hierarchically at /albums/:album_id/:file_id (reached from within an
// album — a child of that album in the URL, not a `?album=` query param,
// so Escape can walk back up this same hierarchy one level at a time:
// photo -> album -> albums list). Both routes render this component; only
// the second has slideshow controls, since only it has a well-defined
// ordered list to move through.

use lestobackupper_schema::protocol::ThumbnailVariant;
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::hooks::{use_navigate, use_params_map};

use crate::api;
use crate::browser;
use crate::context::use_app;
use crate::icons::{self, PENCIL};
use crate::shortcuts::mnemonic;
use crate::thumb_cache;

const DEFAULT_AUTOPLAY_SECONDS: u32 = 5;

#[component]
pub fn PictureView() -> impl IntoView {
    let ctx = use_app();
    let params = use_params_map();
    let navigate = use_navigate();

    let file_id = move || params.get().get("file_id").unwrap_or_default();
    let album_id = move || params.get().get("album_id");

    let image_url: RwSignal<Option<String>> = RwSignal::new(None);
    // True while `image_url` is the Small placeholder rather than the
    // Original — drives a CSS blur that lifts the instant the full image
    // swaps in. This is the "progressive loading" the small thumbnail
    // exists for: something on screen immediately, sharpened once the
    // full bytes actually arrive.
    let loading_full: RwSignal<bool> = RwSignal::new(true);
    let error: RwSignal<Option<String>> = RwSignal::new(None);
    let file_name: RwSignal<Option<String>> = RwSignal::new(None);
    let editing_name: RwSignal<bool> = RwSignal::new(false);
    let name_draft: RwSignal<String> = RwSignal::new(String::new());

    // The current album's file order — empty outside an album context.
    // Refetched only when the album itself changes, not on every photo
    // within it.
    let ordered: RwSignal<Vec<String>> = RwSignal::new(Vec::new());
    let autoplay: RwSignal<bool> = RwSignal::new(false);
    let autoplay_seconds: RwSignal<u32> = RwSignal::new(DEFAULT_AUTOPLAY_SECONDS);

    Effect::new(move |_| {
        let Some(id) = album_id() else {
            ordered.set(Vec::new());
            return;
        };
        let Some(client) = ctx.client.get_untracked() else { return };
        let Some(token) = ctx.token.get_untracked() else { return };
        spawn_local(async move {
            if let Ok(infos) = api::list_album_files(&client, &token, &id).await {
                ordered.set(infos.into_iter().map(|f| f.file_id).collect());
            }
        });
    });

    Effect::new(move |_| {
        let file_id = file_id();
        let Some(client) = ctx.client.get_untracked() else { return };
        let Some(token) = ctx.token.get_untracked() else { return };
        image_url.set(None);
        loading_full.set(true);
        error.set(None);
        file_name.set(None);
        editing_name.set(false);

        // The Small thumbnail is almost always already cached (it's what
        // the grid/album preview the user clicked through from just
        // showed), so this normally paints instantly rather than actually
        // hitting the network a second time.
        let (client_small, token_small, file_id_small) = (client.clone(), token.clone(), file_id.clone());
        spawn_local(async move {
            if let Some(url) = thumb_cache::fetch(&client_small, &token_small, &file_id_small, ThumbnailVariant::Small).await {
                if image_url.get_untracked().is_none() {
                    image_url.set(Some(url));
                }
            }
        });

        spawn_local(async move {
            match api::download_file(&client, &token, &file_id, ThumbnailVariant::Original).await {
                Ok((data, mime, name)) => match browser::object_url_for(&data, &mime) {
                    Ok(url) => {
                        image_url.set(Some(url));
                        loading_full.set(false);
                        file_name.set(name);
                    }
                    Err(_) => error.set(Some("failed to render this file".to_string())),
                },
                Err(message) => error.set(Some(message)),
            }
        });
    });

    let current_index = move || ordered.get().iter().position(|id| *id == file_id());
    let make_href = move |id: &str| match album_id() {
        Some(album) => format!("/albums/{album}/{id}"),
        None => format!("/files/{id}"),
    };
    // Manual Prev/Next stop at the ends of the album; only autoplay loops
    // back to the start, so a slideshow can run unattended while a person
    // clicking through by hand gets an honest "this is the last one".
    let prev_href = move || {
        let list = ordered.get();
        let i = current_index()?;
        (i > 0).then(|| make_href(&list[i - 1]))
    };
    let next_href = move || {
        let list = ordered.get();
        let i = current_index()?;
        (i + 1 < list.len()).then(|| make_href(&list[i + 1]))
    };
    let autoplay_href = move || {
        let list = ordered.get();
        if list.is_empty() {
            return None;
        }
        let i = current_index()?;
        Some(make_href(&list[(i + 1) % list.len()]))
    };

    {
        let navigate = navigate.clone();
        let handle = window_event_listener(leptos::ev::keydown, move |ev| {
            // Gated by the same Settings toggle as grid navigation, and
            // backs off the moment focus is on the autoplay-seconds field
            // (or anything else editable) so its own arrow-key/typing
            // behavior isn't hijacked — see `is_editable_target`'s doc.
            if !ctx.keyboard_shortcuts.get_untracked() || browser::is_editable_target() {
                return;
            }
            match ev.key().as_str() {
                "ArrowLeft" => {
                    if let Some(href) = prev_href() {
                        ev.prevent_default();
                        navigate(&href, Default::default());
                    }
                }
                "ArrowRight" => {
                    if let Some(href) = next_href() {
                        ev.prevent_default();
                        navigate(&href, Default::default());
                    }
                }
                // A photo has no selection of its own to clear, so Escape
                // always walks up one level: to the containing album if
                // there is one, otherwise to Library — the same hierarchy
                // the URL itself encodes.
                "Escape" => {
                    ev.prevent_default();
                    let up = match album_id() {
                        Some(album) => format!("/albums/{album}"),
                        None => "/library".to_string(),
                    };
                    navigate(&up, Default::default());
                }
                _ => {}
            }
        });
        on_cleanup(move || handle.remove());
    }

    // Autoplay: reruns whenever the photo changes, autoplay is
    // toggled, or the interval changes — each run cancels whatever
    // timeout the previous run scheduled (`on_cleanup` inside an
    // `Effect` fires before the next run, not just on unmount) and, if
    // still playing, schedules exactly one more advance.
    {
        // Cloned before the effect closure captures it, so the original
        // `navigate` binding stays available for the Prev/Next buttons
        // below — a `move` closure referencing `navigate.clone()` still
        // moves the outer `navigate` itself into its environment.
        let navigate_for_autoplay = navigate.clone();
        Effect::new(move |_| {
            if !autoplay.get() {
                return;
            }
            let Some(href) = autoplay_href() else { return };
            let seconds = autoplay_seconds.get();
            let navigate = navigate_for_autoplay.clone();
            let handle = browser::set_timeout((seconds.max(1) * 1000) as i32, move || {
                navigate(&href, Default::default());
            });
            on_cleanup(move || browser::clear_timeout(handle));
        });
    }

    let in_album = move || album_id().is_some();

    let start_rename = move |_| {
        name_draft.set(file_name.get_untracked().unwrap_or_default());
        editing_name.set(true);
    };
    let cancel_rename = move |_| editing_name.set(false);
    let save_rename = move || {
        let Some(client) = ctx.client.get_untracked() else { return };
        let Some(token) = ctx.token.get_untracked() else { return };
        let id = file_id();
        let name = name_draft.get();
        if name.trim().is_empty() {
            return;
        }
        spawn_local(async move {
            match api::rename_file(&client, &token, &id, &name).await {
                Ok(_) => {
                    file_name.set(Some(name));
                    editing_name.set(false);
                }
                Err(message) => ctx.status.set(format!("couldn't rename: {message}")),
            }
        });
    };
    let submit_rename = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        save_rename();
    };

    view! {
        <section class="picture-view">
            <div class="toolbar">
                <button class="link" accesskey="b" on:click=move |_| browser::go_back()>{mnemonic("< Back", 'b')}</button>
                <Show when=in_album>
                    <div class="slideshow-controls">
                        <button disabled=move || prev_href().is_none() accesskey="p" on:click={
                            let navigate = navigate.clone();
                            move |_| { if let Some(href) = prev_href() { navigate(&href, Default::default()); } }
                        }>{mnemonic("< Prev", 'p')}</button>
                        <button disabled=move || next_href().is_none() accesskey="x" on:click={
                            let navigate = navigate.clone();
                            move |_| { if let Some(href) = next_href() { navigate(&href, Default::default()); } }
                        }>{mnemonic("Next >", 'x')}</button>
                        <button accesskey="y" on:click=move |_| autoplay.update(|a| *a = !*a)>{move || mnemonic(if autoplay.get() { "Stop autoplay" } else { "Autoplay" }, 'y')}</button>
                        <label class="autoplay-seconds">
                            "every "
                            <input
                                type="number"
                                min="1"
                                prop:value=move || autoplay_seconds.get()
                                on:input:target=move |ev| {
                                    if let Ok(v) = ev.target().value().parse::<u32>() {
                                        autoplay_seconds.set(v);
                                    }
                                }
                            />
                            "s"
                        </label>
                    </div>
                </Show>
            </div>
            <Show when=move || error.get().is_some()>
                <p class="empty">{move || error.get().unwrap_or_default()}</p>
            </Show>
            <Show when=move || image_url.get().is_some()>
                <Show
                    when=move || !editing_name.get()
                    fallback=move || view! {
                        <form class="file-name-row" on:submit=submit_rename>
                            <input
                                type="text"
                                class="file-name-input"
                                autofocus=true
                                prop:value=move || name_draft.get()
                                on:input:target=move |ev| name_draft.set(ev.target().value())
                            />
                            <button type="submit" class="icon-button" aria-label="Save name" title="Save name">"✓"</button>
                            <button type="button" class="icon-button" aria-label="Cancel rename" title="Cancel rename" on:click=cancel_rename>"✕"</button>
                        </form>
                    }
                >
                    <div class="file-name-row">
                        <span class="file-name">{move || file_name.get().unwrap_or_else(|| "Untitled".to_string())}</span>
                        <button type="button" class="icon-button" aria-label="Rename this photo" title="Rename" on:click=start_rename>
                            {icons::icon(PENCIL)}
                        </button>
                    </div>
                </Show>
                <img class:loading-full=move || loading_full.get() src=move || image_url.get().unwrap_or_default() />
            </Show>
        </section>
    }
}
