// The whole client is this WASM binary plus a static index.html — no
// server-side rendering, no build-time backend, nothing but a browser
// pointed at static files talking WebSocket to server-api.
//
// Deliberate departure from ../docs/web.md's original "no upload, browser stays
// preview-only" design: there's no other client yet (no Linux/Android/iOS
// app built), so this is currently the only way to get real photos into
// the system at all. Revisit once a real device client exists.
//
// Every page has its own URL (leptos_router) — /albums/:id and /files/:id
// are bookmarkable/shareable, not just reachable by clicking through from
// a list. Auth gating doesn't care what the URL is: an unauthenticated
// visit to any path just shows the login form, and — since the address
// bar keeps whatever path was requested — logging in then lands exactly
// where the Routes component would render for that URL anyway, with no
// separate "redirect after login" logic needed.

mod activity_log;
mod admin;
mod albums;
mod api;
mod browser;
mod context;
mod grid_nav;
mod icons;
mod library;
mod picture;
mod settings;
mod shortcuts;
mod thumb_cache;
mod ws_client;

use std::rc::Rc;

use context::{use_app, AppContext};
use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::components::{Route, Router, Routes, A};
use leptos_router::hooks::use_location;
use leptos_router::path;
use shortcuts::mnemonic;
use ws_client::WsClient;

const TOKEN_STORAGE_KEY: &str = "lestobackupper-token";
const ADMIN_STORAGE_KEY: &str = "lestobackupper-is-admin";

#[component]
fn App() -> impl IntoView {
    let ctx = AppContext::new();
    provide_context(ctx);

    ctx.status.set("connecting...".to_string());
    // The server this page came from (browser::server_ws_url).
    let server_ws_url = browser::server_ws_url();
    spawn_local(async move {
        match WsClient::connect(&server_ws_url).await {
            Ok(c) => {
                ctx.client.set(Some(Rc::new(c)));
                ctx.status.set(String::new());

                // A reload doesn't have to mean logging in again — but the
                // saved token might be stale (revoked, expired), so it's
                // only trusted after a real call using it succeeds.
                if let Some(saved) = browser::local_storage_get(TOKEN_STORAGE_KEY) {
                    let client = ctx.client.get_untracked().unwrap();
                    if api::list_library(&client, &saved).await.is_ok() {
                        // is_admin travels alongside the token since
                        // there's no separate "am I admin" query — it's
                        // only ever learned from a LoginResponse.
                        let is_admin = browser::local_storage_get(ADMIN_STORAGE_KEY).as_deref() == Some("1");
                        ctx.is_admin.set(is_admin);
                        ctx.token.set(Some(saved));
                    } else {
                        browser::local_storage_remove(TOKEN_STORAGE_KEY);
                        browser::local_storage_remove(ADMIN_STORAGE_KEY);
                    }
                }
            }
            Err(_) => ctx.status.set(format!("failed to connect to {server_ws_url}")),
        }
    });

    // Applied once at startup; Settings flips it again on toggle.
    let dark = browser::local_storage_get("lestobackupper-dark-mode").as_deref() == Some("1");
    browser::set_dark_mode(dark);

    view! {
        <Router>
            <Show when=move || ctx.token.get().is_some() fallback=AuthView>
                <AppShell />
            </Show>
        </Router>
    }
}

#[component]
fn AuthView() -> impl IntoView {
    let ctx = use_app();
    let (username, set_username) = signal(String::new());
    let (password, set_password) = signal(String::new());
    let (otp_code, set_otp_code) = signal(String::new());
    // Set once the server confirms username+password are correct but this
    // account also has TOTP enabled (`api::LoginOutcome::OtpRequired`) —
    // at that point retyping either would be pointless (they're already
    // verified), so they lock and the form's only job becomes collecting
    // the code.
    let otp_required: RwSignal<bool> = RwSignal::new(false);
    let otp_input: NodeRef<leptos::html::Input> = NodeRef::new();

    // `autofocus` (on the input below) is the declarative way to ask for
    // this, but it isn't reliably honored on an element inserted after
    // the initial page load rather than present at parse time — Firefox
    // in particular has historically been inconsistent here. Backed up
    // with an explicit focus so it actually happens everywhere.
    Effect::new(move |_| {
        if otp_required.get() {
            if let Some(input) = otp_input.get() {
                let _ = input.focus();
            }
        }
    });

    let register = move |_| {
        let Some(client) = ctx.client.get() else {
            ctx.status.set("not connected yet".to_string());
            return;
        };
        let (username, password) = (username.get(), password.get());
        spawn_local(async move {
            ctx.status.set("registering...".to_string());
            match api::create_user(&client, &username, &password).await {
                Ok(user_id) => ctx.status.set(format!("registered (user_id {user_id}) — pending admin approval before you can log in")),
                Err(message) => ctx.status.set(format!("registration failed: {message}")),
            }
        });
    };

    let log_in = move || {
        let Some(client) = ctx.client.get() else {
            ctx.status.set("not connected yet".to_string());
            return;
        };
        let (username, password) = (username.get(), password.get());
        // Only ever sent once the OTP step is actually showing — an empty
        // code on the first attempt would just bounce back as
        // OtpRequired again anyway, but there's no reason to send it
        // before the account is even known to need one.
        let otp = otp_required.get_untracked().then(|| otp_code.get());
        spawn_local(async move {
            ctx.status.set("logging in...".to_string());
            match api::login(&client, &username, &password, otp.as_deref()).await {
                api::LoginOutcome::Success { token, is_admin } => {
                    browser::local_storage_set(TOKEN_STORAGE_KEY, &token);
                    browser::local_storage_set(ADMIN_STORAGE_KEY, if is_admin { "1" } else { "0" });
                    ctx.is_admin.set(is_admin);
                    ctx.token.set(Some(token));
                    ctx.status.set(String::new());
                }
                api::LoginOutcome::OtpRequired => {
                    otp_required.set(true);
                    ctx.status.set("enter your one-time code".to_string());
                }
                api::LoginOutcome::Error(message) => ctx.status.set(format!("login failed: {message}")),
            }
        });
    };

    // Cancels the in-progress OTP step and unlocks username/password —
    // for "wrong account, let me start over" without a full page reload.
    let abort = move |_| {
        otp_required.set(false);
        set_otp_code.set(String::new());
        ctx.status.set(String::new());
    };

    // A <form> makes Enter in either field submit — matching "pressing
    // enter should try log in", not Register (registering has to stay an
    // explicit button click, or Enter after typing a username would
    // silently create an account instead of logging into an existing
    // one).
    let on_submit = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        log_in();
    };

    view! {
        <main class="auth">
            <h1>"LestoBackupper"</h1>
            <p class="status">{move || ctx.status.get()}</p>
            <form on:submit=on_submit>
                <input type="text" placeholder="username" prop:disabled=move || otp_required.get() on:input:target=move |ev| set_username.set(ev.target().value()) />
                <input type="password" placeholder="password" prop:disabled=move || otp_required.get() on:input:target=move |ev| set_password.set(ev.target().value()) />
                <Show when=move || otp_required.get()>
                    <input type="text" placeholder="one-time code" autofocus=true node_ref=otp_input prop:value=move || otp_code.get() on:input:target=move |ev| set_otp_code.set(ev.target().value()) />
                </Show>
                // Log in first (the common case), Register last — and
                // replaced entirely by Abort once the OTP step is showing,
                // since registering mid-login makes no sense there.
                <div class="row">
                    <button type="submit" accesskey="l">{mnemonic("Log in", 'l')}</button>
                    <Show when=move || !otp_required.get() fallback=move || view! {
                        <button type="button" accesskey="a" on:click=abort>{mnemonic("Abort", 'a')}</button>
                    }>
                        <button type="button" accesskey="r" on:click=register>{mnemonic("Register", 'r')}</button>
                    </Show>
                </div>
            </form>
            <p class="hint">"First run? The server auto-creates admin/admin — log in with that, then approve other accounts from Admin."</p>
        </main>
    }
}

#[component]
fn AppShell() -> impl IntoView {
    let ctx = use_app();
    let location = use_location();

    let log_out = move |_| {
        browser::local_storage_remove(TOKEN_STORAGE_KEY);
        browser::local_storage_remove(ADMIN_STORAGE_KEY);
        ctx.token.set(None);
        ctx.is_admin.set(false);
    };

    // "/" is also Library (see the Routes below), so it counts as a match
    // for that one nav item too — otherwise landing on the bare root shows
    // the Library page with no nav item highlighted for it.
    let nav_class = move |prefix: &'static str| move || {
        let path = location.pathname.get();
        let is_match = path.starts_with(prefix) || (prefix == "/library" && path == "/");
        if is_match { "active" } else { "" }
    };

    view! {
        <div class="app">
            <nav>
                <span class="brand">"LestoBackupper"</span>
                <A href="/library" attr:class=nav_class("/library") attr:accesskey="l">{mnemonic("Library", 'l')}</A>
                <A href="/albums" attr:class=nav_class("/albums") attr:accesskey="a">{mnemonic("Albums", 'a')}</A>
                <Show when=move || ctx.is_admin.get()>
                    <A href="/admin" attr:class=nav_class("/admin") attr:accesskey="d">{mnemonic("Admin", 'd')}</A>
                </Show>
                <A href="/settings" attr:class=nav_class("/settings") attr:accesskey="t">{mnemonic("Settings", 't')}</A>
                <span class="spacer"></span>
                <button on:click=log_out accesskey="o">{mnemonic("Log out", 'o')}</button>
            </nav>
            <p class="status">{move || ctx.status.get()}</p>
            <main>
                <Routes fallback=|| view! { <p>"Not found"</p> }>
                    <Route path=path!("/") view=library::LibraryView />
                    <Route path=path!("/library") view=library::LibraryView />
                    <Route path=path!("/albums") view=albums::AlbumsView />
                    <Route path=path!("/albums/:album_id") view=albums::AlbumDetailView />
                    // Hierarchical, not a query param (`/files/:id?album=`) —
                    // a photo reached from within an album is a child of that
                    // album in the URL too, and Escape (picture.rs) walks
                    // back up this same hierarchy one level at a time.
                    <Route path=path!("/albums/:album_id/:file_id") view=picture::PictureView />
                    <Route path=path!("/files/:file_id") view=picture::PictureView />
                    <Route path=path!("/admin") view=admin::AdminView />
                    <Route path=path!("/settings") view=settings::SettingsView />
                </Routes>
            </main>
            <activity_log::ActivityLogPanel />
        </div>
    }
}

pub fn main() {
    console_error_panic_hook::set_once();
    leptos::mount::mount_to_body(App);
}
