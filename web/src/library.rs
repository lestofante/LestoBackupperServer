// "All Photos" (server.md: user_file *is* the library, not an album row) —
// every file this account owns, regardless of which albums (if any) it's
// also filed under. This is where an upload always lands first; sorting
// it into albums afterward is optional.

use std::collections::HashMap;

use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::A;
use leptos_router::hooks::use_navigate;
use wasm_bindgen::JsCast;
use web_sys::HtmlInputElement;

use crate::api::{self, AlbumSummary};
use crate::browser;
use crate::context::use_app;
use crate::grid_nav::GridSelection;
use crate::icons::{self, PENCIL};
use crate::shortcuts::mnemonic;
use crate::thumb_cache;

#[component]
pub fn LibraryView() -> impl IntoView {
    let ctx = use_app();
    let files: RwSignal<Vec<String>> = RwSignal::new(Vec::new());
    let names: RwSignal<HashMap<String, Option<String>>> = RwSignal::new(HashMap::new());
    let thumbs: RwSignal<HashMap<String, String>> = RwSignal::new(HashMap::new());
    let albums: RwSignal<Vec<AlbumSummary>> = RwSignal::new(Vec::new());
    let dragging: RwSignal<bool> = RwSignal::new(false);
    // Which file's name is currently being edited, if any — only one tile
    // can be mid-rename at a time, same pattern as an album's rename.
    let renaming: RwSignal<Option<String>> = RwSignal::new(None);
    let rename_draft: RwSignal<String> = RwSignal::new(String::new());
    let navigate = use_navigate();
    let grid = GridSelection::new();

    let refresh = move || {
        let Some(client) = ctx.client.get_untracked() else { return };
        let Some(token) = ctx.token.get_untracked() else { return };
        spawn_local(async move {
            if let Ok(infos) = api::list_library(&client, &token).await {
                files.set(infos.iter().map(|f| f.file_id.clone()).collect());
                names.set(infos.iter().map(|f| (f.file_id.clone(), f.name.clone())).collect());
                for info in infos {
                    if thumbs.get_untracked().contains_key(&info.file_id) {
                        continue;
                    }
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
            if let Ok(a) = api::list_my_albums(&client, &token).await {
                albums.set(a);
            }
        });
    };

    Effect::new(move |_| refresh());

    // No `on_escape_up`: Library is top-level, nothing above it to go to.
    grid.bind_keyboard(
        ctx.keyboard_shortcuts,
        move || files.get(),
        ".tile",
        move |file_id| navigate(&format!("/files/{file_id}"), Default::default()),
        || {},
    );

    let upload_files = move |file_list: web_sys::FileList| {
        let Some(client) = ctx.client.get_untracked() else { return };
        let Some(token) = ctx.token.get_untracked() else { return };
        for i in 0..file_list.length() {
            let Some(file) = file_list.get(i) else { continue };
            let mime = if file.type_().is_empty() { "application/octet-stream".to_string() } else { file.type_() };
            // The browser's own filename — the file's default name until
            // someone renames it (see server::Action::UploadFile's doc
            // comment). Empty for a File synthesized without one, which
            // shouldn't happen for a real drag/drop or file picker.
            let name = (!file.name().is_empty()).then(|| file.name());
            let (client, token) = (client.clone(), token.clone());
            spawn_local(async move {
                ctx.status.set("uploading...".to_string());
                match browser::read_file_bytes(&file).await {
                    Ok(data) => match api::upload_file(&client, &token, data, &mime, name.as_deref()).await {
                        Ok(_) => {
                            ctx.status.set(String::new());
                            refresh();
                        }
                        Err(message) => ctx.status.set(format!("upload failed: {message}")),
                    },
                    Err(_) => ctx.status.set("failed to read the selected file".to_string()),
                }
            });
        }
    };

    let on_input_change = move |ev: leptos::ev::Event| {
        let Some(input) = ev.target().and_then(|t| t.dyn_into::<HtmlInputElement>().ok()) else { return };
        if let Some(files) = input.files() {
            upload_files(files);
        }
    };

    let on_drop = move |ev: leptos::ev::DragEvent| {
        ev.prevent_default();
        dragging.set(false);
        if let Some(files) = ev.data_transfer().and_then(|dt| dt.files()) {
            upload_files(files);
        }
    };

    let add_to_album = move |file_id: String, album_id: String| {
        let Some(client) = ctx.client.get_untracked() else { return };
        let Some(token) = ctx.token.get_untracked() else { return };
        spawn_local(async move {
            if let Err(message) = api::add_to_album(&client, &token, &album_id, &file_id).await {
                ctx.status.set(format!("couldn't add to album: {message}"));
            } else {
                ctx.flash("added to album");
            }
        });
    };

    let add_selected_to_album = move |ev: leptos::ev::Event| {
        let album_id = event_target_value(&ev);
        if album_id.is_empty() {
            return;
        }
        let Some(client) = ctx.client.get_untracked() else { return };
        let Some(token) = ctx.token.get_untracked() else { return };
        let selected: Vec<String> = grid.selected.get_untracked().into_iter().collect();
        spawn_local(async move {
            for file_id in selected {
                let _ = api::add_to_album(&client, &token, &album_id, &file_id).await;
            }
            ctx.flash("added to album");
            grid.clear_selection();
        });
    };

    let start_rename = move |file_id: String| {
        let current = names.get_untracked().get(&file_id).cloned().flatten().unwrap_or_default();
        rename_draft.set(current);
        renaming.set(Some(file_id));
    };
    let cancel_rename = move |_| renaming.set(None);
    let save_rename = move || {
        let Some(client) = ctx.client.get_untracked() else { return };
        let Some(token) = ctx.token.get_untracked() else { return };
        let Some(file_id) = renaming.get_untracked() else { return };
        let name = rename_draft.get();
        if name.trim().is_empty() {
            return;
        }
        spawn_local(async move {
            match api::rename_file(&client, &token, &file_id, &name).await {
                Ok(_) => {
                    names.update(|n| {
                        n.insert(file_id, Some(name));
                    });
                    renaming.set(None);
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
        <section class="library">
            <div class="toolbar">
                <label class="upload-button" accesskey="p">
                    {mnemonic("Upload", 'p')}
                    <input type="file" accept="image/*,video/*" multiple=true on:change=on_input_change style="display: none" />
                </label>
            </div>
            <Show when=move || grid.has_selection()>
                <div class="bulk-bar">
                    <span class="count">{move || format!("{} selected", grid.selected_count())}</span>
                    <Show when=move || !albums.get().is_empty()>
                        <select on:change=add_selected_to_album>
                            <option value="">"Add selected to album..."</option>
                            <For each=move || albums.get() key=|a| a.album_id.clone() let:album>
                                <option value=album.album_id.clone()>{album.name.clone()}</option>
                            </For>
                        </select>
                    </Show>
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
                "Or drag photos here"
            </div>
            <Show when=move || files.get().is_empty()>
                <p class="empty">"No photos yet — upload one to get started."</p>
            </Show>
            <div class="grid">
                <For each=move || files.get() key=|id| id.clone() let:file_id>
                    {
                        let file_id_for_select = file_id.clone();
                        let file_id_for_click = file_id.clone();
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
                                let file_id_for_click = file_id_for_click.clone();
                                move || {
                                    let file_id_for_click = file_id_for_click.clone();
                                    thumbs
                                        .get()
                                        .get(&file_id)
                                        .cloned()
                                        .map(|url| {
                                            let href = format!("/files/{file_id}");
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
                                    if renaming.get().as_deref() == Some(file_id_for_show.as_str()) {
                                        view! {
                                            <form class="file-name-row" on:submit=submit_rename>
                                                <input
                                                    type="text"
                                                    class="file-name-input"
                                                    autofocus=true
                                                    prop:value=move || rename_draft.get()
                                                    on:input:target=move |ev| rename_draft.set(ev.target().value())
                                                />
                                                <button type="submit" class="icon-button" aria-label="Save name" title="Save name">"✓"</button>
                                                <button type="button" class="icon-button" aria-label="Cancel rename" title="Cancel rename" on:click=cancel_rename>"✕"</button>
                                            </form>
                                        }.into_any()
                                    } else {
                                        let display_name = names.get().get(&file_id_for_show).cloned().flatten().unwrap_or_else(|| "Untitled".to_string());
                                        view! {
                                            <div class="file-name-row">
                                                <span class="file-name" title=display_name.clone()>{display_name.clone()}</span>
                                                // No `accesskey` here — this button repeats
                                                // once per tile, and accesskeys only make
                                                // sense for a control that appears once per
                                                // page (a real letter can only route to one
                                                // element at a time).
                                                <button
                                                    type="button"
                                                    class="icon-button"
                                                    aria-label="Rename this photo"
                                                    title="Rename"
                                                    on:click=move |_| start_rename(file_id_for_start.clone())
                                                >
                                                    {icons::icon(PENCIL)}
                                                </button>
                                            </div>
                                        }.into_any()
                                    }
                                }
                            }
                            <Show when=move || !albums.get().is_empty()>
                                <select on:change={
                                    let file_id = file_id_for_select.clone();
                                    move |ev| {
                                        let album_id = event_target_value(&ev);
                                        if !album_id.is_empty() {
                                            add_to_album(file_id.clone(), album_id);
                                        }
                                    }
                                }>
                                    <option value="">"Add to album..."</option>
                                    <For each=move || albums.get() key=|a| a.album_id.clone() let:album>
                                        <option value=album.album_id.clone()>{album.name.clone()}</option>
                                    </For>
                                </select>
                            </Show>
                        </div>
                        }
                    }
                </For>
            </div>
        </section>
    }
}
