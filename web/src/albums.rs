use std::collections::HashMap;

use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::A;
use leptos_router::hooks::{use_navigate, use_params_map};

use crate::api::{self, AlbumSummary};
use crate::browser;
use crate::context::use_app;
use crate::grid_nav::GridSelection;
use crate::icons::{self, PENCIL, RESTORE, TRASH};
use crate::shortcuts::mnemonic;
use crate::thumb_cache;

const PREVIEW_COUNT: usize = 4;

#[derive(Clone, PartialEq)]
struct AlbumWithPreview {
    album_id: String,
    name: String,
    thumbs: Vec<String>,
}

#[component]
pub fn AlbumsView() -> impl IntoView {
    let ctx = use_app();
    let previews: RwSignal<Vec<AlbumWithPreview>> = RwSignal::new(Vec::new());
    let (new_name, set_new_name) = signal(String::new());
    let show_hidden: RwSignal<bool> = RwSignal::new(false);
    let hidden_albums: RwSignal<Vec<AlbumSummary>> = RwSignal::new(Vec::new());
    let navigate = use_navigate();
    let grid = GridSelection::new();

    let refresh = move || {
        let Some(client) = ctx.client.get_untracked() else { return };
        let Some(token) = ctx.token.get_untracked() else { return };
        spawn_local(async move {
            let Ok(albums) = api::list_my_albums(&client, &token).await else { return };
            previews.set(albums.iter().map(|a| AlbumWithPreview { album_id: a.album_id.clone(), name: a.name.clone(), thumbs: Vec::new() }).collect());

            for album in albums {
                let (client, token) = (client.clone(), token.clone());
                spawn_local(async move {
                    let Ok(infos) = api::list_album_files(&client, &token, &album.album_id).await else { return };
                    for info in infos.into_iter().take(PREVIEW_COUNT) {
                        let (client, token, album_id) = (client.clone(), token.clone(), album.album_id.clone());
                        spawn_local(async move {
                            if let Some(url) = thumb_cache::fetch(&client, &token, &info.file_id, thumb_cache::variant_for_grid()).await {
                                previews.update(|list| {
                                    if let Some(entry) = list.iter_mut().find(|a| a.album_id == album_id) {
                                        entry.thumbs.push(url);
                                    }
                                });
                            }
                        });
                    }
                });
            }
        });
    };
    Effect::new(move |_| refresh());

    let album_ids = move || previews.get().into_iter().map(|a| a.album_id).collect::<Vec<_>>();
    {
        let navigate = navigate.clone();
        // No `on_escape_up`: the albums list is top-level, nothing above
        // it to go to.
        grid.bind_keyboard(
            ctx.keyboard_shortcuts,
            album_ids,
            ".album-card",
            move |album_id| navigate(&format!("/albums/{album_id}"), Default::default()),
            || {},
        );
    }

    let refresh_hidden = move || {
        let Some(client) = ctx.client.get_untracked() else { return };
        let Some(token) = ctx.token.get_untracked() else { return };
        spawn_local(async move {
            if let Ok(albums) = api::list_hidden_albums(&client, &token).await {
                hidden_albums.set(albums);
            }
        });
    };

    let toggle_hidden = move |_| {
        let now_showing = !show_hidden.get();
        show_hidden.set(now_showing);
        if now_showing {
            refresh_hidden();
        }
    };

    let unhide = move |album_id: String| {
        let Some(client) = ctx.client.get_untracked() else { return };
        let Some(token) = ctx.token.get_untracked() else { return };
        spawn_local(async move {
            match api::unhide_album(&client, &token, &album_id).await {
                Ok(_) => {
                    refresh();
                    refresh_hidden();
                }
                Err(message) => ctx.status.set(format!("couldn't unhide album: {message}")),
            }
        });
    };

    let hide = move |album_id: String| {
        let Some(client) = ctx.client.get_untracked() else { return };
        let Some(token) = ctx.token.get_untracked() else { return };
        spawn_local(async move {
            match api::hide_album(&client, &token, &album_id).await {
                Ok(_) => refresh(),
                Err(message) => ctx.status.set(format!("couldn't hide album: {message}")),
            }
        });
    };

    let hide_selected = move |_| {
        let Some(client) = ctx.client.get_untracked() else { return };
        let Some(token) = ctx.token.get_untracked() else { return };
        let ids: Vec<String> = grid.selected.get_untracked().into_iter().collect();
        spawn_local(async move {
            for id in ids {
                let _ = api::hide_album(&client, &token, &id).await;
            }
            grid.clear_selection();
            refresh();
        });
    };

    // Deliberately doesn't navigate into the new album — the user stays on
    // the list and sees it appear there, rather than being dropped
    // straight into an empty album they didn't ask to open.
    let create = move || {
        let Some(client) = ctx.client.get_untracked() else { return };
        let Some(token) = ctx.token.get_untracked() else { return };
        let name = new_name.get();
        if name.trim().is_empty() {
            return;
        }
        spawn_local(async move {
            match api::create_album(&client, &token, &name).await {
                Ok(_) => {
                    set_new_name.set(String::new());
                    refresh();
                }
                Err(message) => ctx.status.set(format!("couldn't create album: {message}")),
            }
        });
    };
    let submit_create = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        create();
    };

    view! {
        <section class="albums">
            <form class="toolbar" on:submit=submit_create>
                <input type="text" placeholder="new album name" prop:value=move || new_name.get() on:input:target=move |ev| set_new_name.set(ev.target().value()) />
                <button type="submit" accesskey="c">{mnemonic("Create album", 'c')}</button>
            </form>
            <Show when=move || grid.has_selection()>
                <div class="bulk-bar">
                    <span class="count">{move || format!("{} selected", grid.selected_count())}</span>
                    <button on:click=hide_selected accesskey="h">{mnemonic("Hide selected", 'h')}</button>
                    <button class="link" on:click=move |_| grid.clear_selection()>{mnemonic("Clear", 'c')}</button>
                </div>
            </Show>
            <Show when=move || previews.get().is_empty()>
                <p class="empty">"No albums yet."</p>
            </Show>
            <div class="album-grid">
                // `let:album` from `<For>` hands each row a plain, one-time
                // snapshot — reading `album.thumbs` directly here would
                // freeze the preview at whatever it was when the row was
                // first created (empty, before any thumbnail has loaded)
                // and never update, since same-keyed rows aren't recreated
                // as `previews` changes. Reading `previews.get()` *inside*
                // each slot's closure instead makes that closure a real
                // subscriber, so it reruns every time a thumbnail arrives.
                <For each=move || previews.get() key=|a| a.album_id.clone() let:album>
                    {
                        let album_id = album.album_id.clone();
                        let href = format!("/albums/{album_id}");
                        // Cloned up front for the Hide button and the
                        // click/focus/selection checks below: the
                        // `.map()` closure further down takes its own
                        // move-capture of `album_id` (needed to satisfy
                        // the 'static bound `collect_view()` requires),
                        // which would otherwise leave nothing here to
                        // clone from.
                        let album_id_for_hide = album_id.clone();
                        let album_id_for_click = album_id.clone();
                        let is_selected = {
                            let album_id = album_id.clone();
                            move || grid.is_selected(&album_id)
                        };
                        let is_focused = {
                            let album_id = album_id.clone();
                            move || grid.focused_id(&album_ids()).as_deref() == Some(album_id.as_str())
                        };
                        view! {
                            <div class="album-card" class:grid-selected=is_selected class:grid-focused=is_focused>
                                <A href=href on:click=move |ev| {
                                    let ids = album_ids();
                                    if let Some(index) = ids.iter().position(|id| *id == album_id_for_click) {
                                        if grid.handle_tile_click(&ev, &ids, index) {
                                            ev.prevent_default();
                                        }
                                    }
                                }>
                                    <div class="album-preview">
                                        {(0..PREVIEW_COUNT)
                                            .map(|i| {
                                                let album_id = album_id.clone();
                                                view! {
                                                    {move || {
                                                        let thumb = previews.get().into_iter().find(|a| a.album_id == album_id).and_then(|a| a.thumbs.get(i).cloned());
                                                        match thumb {
                                                            Some(url) => view! { <img src=url /> }.into_any(),
                                                            None => view! { <div class="empty-slot"></div> }.into_any(),
                                                        }
                                                    }}
                                                }
                                            })
                                            .collect_view()}
                                    </div>
                                    <div class="name">{album.name.clone()}</div>
                                </A>
                                <button
                                    type="button"
                                    class="icon-button album-card-hide"
                                    aria-label="Hide this album"
                                    title="Hide"
                                    on:click=move |_| hide(album_id_for_hide.clone())
                                >
                                    {icons::icon(TRASH)}
                                </button>
                            </div>
                        }
                    }
                </For>
            </div>
            <p class="hidden-toggle"><button class="link" accesskey="n" on:click=toggle_hidden>{move || mnemonic(if show_hidden.get() { "Hide the hidden-albums list" } else { "Show hidden albums" }, 'n')}</button></p>
            <Show when=move || show_hidden.get()>
                <div class="panel">
                    <h2>"Hidden albums"</h2>
                    <Show when=move || hidden_albums.get().is_empty()>
                        <p class="empty">"No hidden albums."</p>
                    </Show>
                    <ul class="pending-list">
                        <For each=move || hidden_albums.get() key=|a| a.album_id.clone() let:album>
                            <li>
                                <span>{album.name.clone()}</span>
                                <button
                                    type="button"
                                    class="icon-button"
                                    aria-label="Unhide this album"
                                    title="Unhide"
                                    on:click={
                                        let album_id = album.album_id.clone();
                                        move |_| unhide(album_id.clone())
                                    }
                                >
                                    {icons::icon(RESTORE)}
                                </button>
                            </li>
                        </For>
                    </ul>
                </div>
            </Show>
        </section>
    }
}

#[component]
pub fn AlbumDetailView() -> impl IntoView {
    let ctx = use_app();
    let params = use_params_map();
    let album_id = move || params.get().get("album_id").unwrap_or_default();
    let navigate = use_navigate();

    let album_name: RwSignal<String> = RwSignal::new(String::new());
    let editing_name: RwSignal<bool> = RwSignal::new(false);
    let name_draft: RwSignal<String> = RwSignal::new(String::new());
    let files: RwSignal<Vec<String>> = RwSignal::new(Vec::new());
    let file_names: RwSignal<HashMap<String, Option<String>>> = RwSignal::new(HashMap::new());
    let thumbs: RwSignal<HashMap<String, String>> = RwSignal::new(HashMap::new());
    let dragging: RwSignal<bool> = RwSignal::new(false);
    // Which file's name is being edited, if any — mirrors library.rs's
    // per-tile rename state (a file's name is global, see
    // server::Action::RenameFile's doc comment, so renaming it here
    // changes what it's called everywhere else it's visible too).
    let renaming_file: RwSignal<Option<String>> = RwSignal::new(None);
    let file_name_draft: RwSignal<String> = RwSignal::new(String::new());
    let grid = GridSelection::new();

    let refresh = move || {
        let Some(client) = ctx.client.get_untracked() else { return };
        let Some(token) = ctx.token.get_untracked() else { return };
        let id = album_id();
        thumbs.set(HashMap::new());
        spawn_local(async move {
            if let Ok(albums) = api::list_my_albums(&client, &token).await {
                if let Some(a) = albums.into_iter().find(|a| a.album_id == id) {
                    album_name.set(a.name);
                }
            }
            if let Ok(infos) = api::list_album_files(&client, &token, &id).await {
                files.set(infos.iter().map(|f| f.file_id.clone()).collect());
                file_names.set(infos.iter().map(|f| (f.file_id.clone(), f.name.clone())).collect());
                for info in infos {
                    let (client, token, file_id) = (client.clone(), token.clone(), info.file_id);
                    spawn_local(async move {
                        if let Some(url) = thumb_cache::fetch(&client, &token, &file_id, thumb_cache::variant_for_grid()).await {
                            thumbs.update(|t| {
                                t.insert(file_id, url);
                            });
                        }
                    });
                }
            }
        });
    };
    Effect::new(move |_| refresh());

    {
        let navigate = navigate.clone();
        let navigate_up = navigate.clone();
        // Escape with nothing selected goes up to the albums list — one
        // level, matching the URL's own /albums/:album_id/:file_id ->
        // /albums/:album_id -> /albums hierarchy.
        grid.bind_keyboard(
            ctx.keyboard_shortcuts,
            move || files.get(),
            ".tile",
            move |file_id| navigate(&format!("/albums/{}/{file_id}", album_id()), Default::default()),
            move || navigate_up("/albums", Default::default()),
        );
    }

    let start_rename = move |_| {
        name_draft.set(album_name.get_untracked());
        editing_name.set(true);
    };

    let save_rename = move || {
        let Some(client) = ctx.client.get_untracked() else { return };
        let Some(token) = ctx.token.get_untracked() else { return };
        let id = album_id();
        let name = name_draft.get();
        if name.trim().is_empty() {
            return;
        }
        spawn_local(async move {
            match api::rename_album(&client, &token, &id, &name).await {
                Ok(_) => {
                    editing_name.set(false);
                    refresh();
                }
                Err(message) => ctx.status.set(format!("couldn't rename album: {message}")),
            }
        });
    };
    let submit_rename = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        save_rename();
    };

    let hide_this_album = move |_| {
        let Some(client) = ctx.client.get_untracked() else { return };
        let Some(token) = ctx.token.get_untracked() else { return };
        let id = album_id();
        let navigate = navigate.clone();
        spawn_local(async move {
            match api::hide_album(&client, &token, &id).await {
                // It no longer shows up in the albums list once hidden, so
                // staying on this page would be a dead end — back to the
                // list, matching the file "Hide" button's own behavior of
                // just removing the tile in place.
                Ok(_) => navigate("/albums", Default::default()),
                Err(message) => ctx.status.set(format!("couldn't hide album: {message}")),
            }
        });
    };

    let trash = move |file_id: String| {
        let Some(client) = ctx.client.get_untracked() else { return };
        let Some(token) = ctx.token.get_untracked() else { return };
        let id = album_id();
        spawn_local(async move {
            match api::trash_from_album(&client, &token, &id, &file_id).await {
                Ok(_) => refresh(),
                Err(message) => ctx.status.set(format!("couldn't hide file: {message}")),
            }
        });
    };

    let hide_selected = move |_| {
        let Some(client) = ctx.client.get_untracked() else { return };
        let Some(token) = ctx.token.get_untracked() else { return };
        let id = album_id();
        let selected: Vec<String> = grid.selected.get_untracked().into_iter().collect();
        spawn_local(async move {
            for file_id in selected {
                let _ = api::trash_from_album(&client, &token, &id, &file_id).await;
            }
            grid.clear_selection();
            refresh();
        });
    };

    let start_rename_file = move |file_id: String| {
        let current = file_names.get_untracked().get(&file_id).cloned().flatten().unwrap_or_default();
        file_name_draft.set(current);
        renaming_file.set(Some(file_id));
    };
    let cancel_rename_file = move |_| renaming_file.set(None);
    let save_rename_file = move || {
        let Some(client) = ctx.client.get_untracked() else { return };
        let Some(token) = ctx.token.get_untracked() else { return };
        let Some(file_id) = renaming_file.get_untracked() else { return };
        let name = file_name_draft.get();
        if name.trim().is_empty() {
            return;
        }
        spawn_local(async move {
            match api::rename_file(&client, &token, &file_id, &name).await {
                Ok(_) => {
                    file_names.update(|n| {
                        n.insert(file_id, Some(name));
                    });
                    renaming_file.set(None);
                }
                Err(message) => ctx.status.set(format!("couldn't rename: {message}")),
            }
        });
    };
    let submit_rename_file = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        save_rename_file();
    };

    // Uploading directly into an album: register the bytes as a file, then
    // immediately add it to *this* album — the album view's drop zone is a
    // shortcut for "upload and file it here" in one step, unlike Library's
    // (which only adds to the account-wide pool).
    let upload_files = move |file_list: web_sys::FileList| {
        let Some(client) = ctx.client.get_untracked() else { return };
        let Some(token) = ctx.token.get_untracked() else { return };
        let id = album_id();
        for i in 0..file_list.length() {
            let Some(file) = file_list.get(i) else { continue };
            let mime = if file.type_().is_empty() { "application/octet-stream".to_string() } else { file.type_() };
            let name = (!file.name().is_empty()).then(|| file.name());
            let (client, token, id) = (client.clone(), token.clone(), id.clone());
            spawn_local(async move {
                ctx.status.set("uploading...".to_string());
                let Ok(data) = browser::read_file_bytes(&file).await else {
                    ctx.status.set("failed to read the selected file".to_string());
                    return;
                };
                match api::upload_file(&client, &token, data, &mime, name.as_deref()).await {
                    Ok(file_id) => match api::add_to_album(&client, &token, &id, &file_id).await {
                        Ok(_) => {
                            ctx.status.set(String::new());
                            refresh();
                        }
                        Err(message) => ctx.status.set(format!("uploaded, but couldn't add to album: {message}")),
                    },
                    Err(message) => ctx.status.set(format!("upload failed: {message}")),
                }
            });
        }
    };

    let on_drop = move |ev: leptos::ev::DragEvent| {
        ev.prevent_default();
        dragging.set(false);
        if let Some(files) = ev.data_transfer().and_then(|dt| dt.files()) {
            upload_files(files);
        }
    };

    view! {
        <section class="album-detail">
            <div class="toolbar album-toolbar">
                <A href="/albums" attr:class="link back-link">"< Albums"</A>
                <Show when=move || !editing_name.get() fallback=move || {
                    // Cloned per-call, not moved: `fallback` itself must
                    // stay `Fn` (Show re-invokes it every time the toggle
                    // flips back), so each call needs its own fresh
                    // `submit_rename` to hand to `on:submit` rather than
                    // permanently consuming the one captured copy.
                    let submit_rename = submit_rename;
                    view! {
                        <form class="album-name-form" on:submit=submit_rename>
                            <input type="text" class="rename-input" autofocus=true prop:value=move || name_draft.get() on:input:target=move |ev| name_draft.set(ev.target().value()) />
                            <button type="submit" accesskey="s">{mnemonic("Save", 's')}</button>
                            <button type="button" class="link" accesskey="c" on:click=move |_| editing_name.set(false)>{mnemonic("Cancel", 'c')}</button>
                        </form>
                    }
                }>
                    {
                        let start_rename = start_rename;
                        let hide_this_album = hide_this_album.clone();
                        view! {
                            <div class="album-name-row">
                                <h2>{move || album_name.get()}</h2>
                                <button type="button" class="icon-button" aria-label="Rename this album" title="Rename" on:click=start_rename>
                                    {icons::icon(PENCIL)}
                                </button>
                                <button type="button" class="icon-button" aria-label="Hide this album" title="Hide" on:click=hide_this_album>
                                    {icons::icon(TRASH)}
                                </button>
                            </div>
                        }
                    }
                </Show>
            </div>
            <Show when=move || grid.has_selection()>
                <div class="bulk-bar">
                    <span class="count">{move || format!("{} selected", grid.selected_count())}</span>
                    <button on:click=hide_selected accesskey="h">{mnemonic("Hide selected", 'h')}</button>
                    <button class="link" on:click=move |_| grid.clear_selection()>{mnemonic("Clear", 'c')}</button>
                </div>
            </Show>
            <div
                class="dropzone"
                class:drag-over=move || dragging.get()
                on:dragover=move |ev| { ev.prevent_default(); dragging.set(true); }
                on:dragleave=move |_| dragging.set(false)
                on:drop=on_drop
            >
                "Drag photos here to add them to this album"
            </div>
            <Show when=move || files.get().is_empty()>
                <p class="empty">"No photos in this album yet."</p>
            </Show>
            <div class="grid">
                <For each=move || files.get() key=|id| id.clone() let:file_id>
                    {
                    let file_id_for_trash = file_id.clone();
                    let file_id_for_name = file_id.clone();
                    view! {
                    <div
                        class="tile"
                        class:grid-selected={
                            let file_id = file_id.clone();
                            move || grid.is_selected(&file_id)
                        }
                        class:grid-focused={
                            let file_id = file_id.clone();
                            move || grid.focused_id(&files.get()).as_deref() == Some(file_id.as_str())
                        }
                    >
                        {
                            let file_id = file_id.clone();
                            let file_id_for_click = file_id.clone();
                            let album_id = album_id();
                            move || {
                                let file_id_for_click = file_id_for_click.clone();
                                thumbs
                                    .get()
                                    .get(&file_id)
                                    .cloned()
                                    .map(|url| {
                                        // Hierarchical, not /files/:id?album=
                                        // — a child of this album in the URL,
                                        // so PictureView can offer prev/next
                                        // through the same list and Escape
                                        // can walk back up to it.
                                        let href = format!("/albums/{album_id}/{file_id}");
                                        view! {
                                            <A href=href on:click=move |ev| {
                                                let ids = files.get();
                                                if let Some(index) = ids.iter().position(|id| *id == file_id_for_click) {
                                                    if grid.handle_tile_click(&ev, &ids, index) {
                                                        ev.prevent_default();
                                                    }
                                                }
                                            }><img src=url /></A>
                                        }.into_any()
                                    })
                                    .unwrap_or_else(|| view! { <div class="placeholder">"loading..."</div> }.into_any())
                            }
                        }
                        {
                            let file_id_for_start = file_id_for_name.clone();
                            let file_id_for_show = file_id_for_name.clone();
                            move || {
                                let file_id_for_start = file_id_for_start.clone();
                                if renaming_file.get().as_deref() == Some(file_id_for_show.as_str()) {
                                    view! {
                                        <form class="file-name-row" on:submit=submit_rename_file>
                                            <input
                                                type="text"
                                                class="file-name-input"
                                                autofocus=true
                                                prop:value=move || file_name_draft.get()
                                                on:input:target=move |ev| file_name_draft.set(ev.target().value())
                                            />
                                            <button type="submit" class="icon-button" aria-label="Save name" title="Save name">"✓"</button>
                                            <button type="button" class="icon-button" aria-label="Cancel rename" title="Cancel rename" on:click=cancel_rename_file>"✕"</button>
                                        </form>
                                    }.into_any()
                                } else {
                                    let display_name = file_names.get().get(&file_id_for_show).cloned().flatten().unwrap_or_else(|| "Untitled".to_string());
                                    view! {
                                        <div class="file-name-row">
                                            <span class="file-name" title=display_name.clone()>{display_name.clone()}</span>
                                            <button
                                                type="button"
                                                class="icon-button"
                                                aria-label="Rename this photo"
                                                title="Rename"
                                                on:click=move |_| start_rename_file(file_id_for_start.clone())
                                            >
                                                {icons::icon(PENCIL)}
                                            </button>
                                        </div>
                                    }.into_any()
                                }
                            }
                        }
                        <button
                            type="button"
                            class="icon-button"
                            aria-label="Hide this photo from the album"
                            title="Hide"
                            on:click=move |_| trash(file_id_for_trash.clone())
                        >
                            {icons::icon(TRASH)}
                        </button>
                    </div>
                    }
                    }
                </For>
            </div>
        </section>
    }
}
