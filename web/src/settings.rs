use leptos::prelude::*;
use leptos::task::spawn_local;
use qrcode::render::svg;
use qrcode::QrCode;

use crate::api;
use crate::browser;
use crate::context::{use_app, KEYBOARD_SHORTCUTS_KEY};
use crate::shortcuts::mnemonic;

const DARK_MODE_KEY: &str = "lestobackupper-dark-mode";

/// `None` on a malformed `otpauth://` URL, which shouldn't happen since
/// the server generated it — but a broken QR render is a worse failure
/// mode than falling back to "no image, here's the text", so this stays a
/// plain `Option` the view can `Show` around rather than a `.unwrap()`.
fn otp_qr_svg(otpauth_url: &str) -> Option<String> {
    let code = QrCode::new(otpauth_url.as_bytes()).ok()?;
    Some(code.render::<svg::Color>().min_dimensions(180, 180).build())
}

#[component]
pub fn SettingsView() -> impl IntoView {
    let ctx = use_app();

    let (dark, set_dark) = signal(browser::local_storage_get(DARK_MODE_KEY).as_deref() == Some("1"));
    let toggle_dark = move |_| {
        let enabled = !dark.get();
        set_dark.set(enabled);
        browser::local_storage_set(DARK_MODE_KEY, if enabled { "1" } else { "0" });
        browser::set_dark_mode(enabled);
    };

    let (old_password, set_old_password) = signal(String::new());
    let (new_password, set_new_password) = signal(String::new());
    let (password_status, set_password_status) = signal(String::new());
    let change_password = move || {
        let Some(client) = ctx.client.get_untracked() else { return };
        let Some(token) = ctx.token.get_untracked() else { return };
        let (old, new) = (old_password.get(), new_password.get());
        spawn_local(async move {
            match api::change_password(&client, &token, &old, &new).await {
                Ok(_) => {
                    set_password_status.set("password changed".to_string());
                    set_old_password.set(String::new());
                    set_new_password.set(String::new());
                }
                Err(message) => set_password_status.set(format!("failed: {message}")),
            }
        });
    };
    let submit_change_password = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        change_password();
    };

    let (otp_secret, set_otp_secret) = signal(Option::<(String, String)>::None); // (secret_base32, otpauth_url)
    let (otp_code, set_otp_code) = signal(String::new());
    let (otp_status, set_otp_status) = signal(String::new());

    let begin_otp = move |_| {
        let Some(client) = ctx.client.get_untracked() else { return };
        let Some(token) = ctx.token.get_untracked() else { return };
        spawn_local(async move {
            match api::begin_otp_enrollment(&client, &token).await {
                Ok((secret, url)) => set_otp_secret.set(Some((secret, url))),
                Err(message) => set_otp_status.set(format!("failed: {message}")),
            }
        });
    };

    let confirm_otp = move || {
        let Some(client) = ctx.client.get_untracked() else { return };
        let Some(token) = ctx.token.get_untracked() else { return };
        let Some((secret, _)) = otp_secret.get() else { return };
        let code = otp_code.get();
        spawn_local(async move {
            match api::confirm_otp_enrollment(&client, &token, &secret, &code).await {
                Ok(_) => {
                    set_otp_status.set("2FA enabled".to_string());
                    set_otp_secret.set(None);
                    set_otp_code.set(String::new());
                }
                Err(message) => set_otp_status.set(format!("failed: {message}")),
            }
        });
    };
    let submit_confirm_otp = move |ev: leptos::ev::SubmitEvent| {
        ev.prevent_default();
        confirm_otp();
    };

    let disable_otp = move |_| {
        let Some(client) = ctx.client.get_untracked() else { return };
        let Some(token) = ctx.token.get_untracked() else { return };
        spawn_local(async move {
            match api::disable_otp(&client, &token).await {
                Ok(_) => set_otp_status.set("2FA disabled".to_string()),
                Err(message) => set_otp_status.set(format!("failed: {message}")),
            }
        });
    };

    let (kb_enabled, set_kb_enabled) = signal(ctx.keyboard_shortcuts.get_untracked());
    let toggle_kb = move |_| {
        let enabled = !kb_enabled.get();
        set_kb_enabled.set(enabled);
        ctx.keyboard_shortcuts.set(enabled);
        browser::local_storage_set(KEYBOARD_SHORTCUTS_KEY, if enabled { "1" } else { "0" });
    };

    view! {
        <section class="settings">
            <div class="panel">
                <h2>"Appearance"</h2>
                <label class="checkbox-row"><input type="checkbox" prop:checked=move || dark.get() on:change=toggle_dark accesskey="m" /> {mnemonic("Dark mode", 'm')}</label>
            </div>

            <div class="panel">
                <h2>"Keyboard"</h2>
                <label class="checkbox-row"><input type="checkbox" prop:checked=move || kb_enabled.get() on:change=toggle_kb accesskey="k" /> {mnemonic("Enable keyboard shortcuts", 'k')}</label>
                <p class="hint">"Access-key underlines (Alt+letter), and arrow-key navigation / multi-select in Library and Albums. Turn this off if it gets in the way of your browser's own shortcuts."</p>
            </div>

            <div class="panel">
                <h2>"Change password"</h2>
                <form on:submit=submit_change_password>
                    <input type="password" placeholder="current password" prop:value=move || old_password.get() on:input:target=move |ev| set_old_password.set(ev.target().value()) />
                    <input type="password" placeholder="new password" prop:value=move || new_password.get() on:input:target=move |ev| set_new_password.set(ev.target().value()) />
                    <button type="submit" accesskey="c">{mnemonic("Change password", 'c')}</button>
                </form>
                <p class="status">{move || password_status.get()}</p>
            </div>

            <div class="panel">
                <h2>"Two-factor authentication"</h2>
                <Show
                    when=move || otp_secret.get().is_some()
                    fallback=move || view! {
                        <div class="row">
                            <button on:click=begin_otp accesskey="e">{mnemonic("Enable 2FA", 'e')}</button>
                            <button on:click=disable_otp accesskey="i">{mnemonic("Disable 2FA", 'i')}</button>
                        </div>
                    }
                >
                    {move || {
                        let (secret, url) = otp_secret.get().unwrap();
                        let qr = otp_qr_svg(&url);
                        view! {
                            <p>"Scan with your authenticator app:"</p>
                            {qr.map(|svg| view! { <div class="qr-code" inner_html=svg></div> })}
                            <p>"Or enter this secret manually:"</p>
                            <p><code>{secret}</code></p>
                            <form on:submit=submit_confirm_otp>
                                <input type="text" placeholder="6-digit code" prop:value=move || otp_code.get() on:input:target=move |ev| set_otp_code.set(ev.target().value()) />
                                <button type="submit" accesskey="o">{mnemonic("Confirm", 'o')}</button>
                            </form>
                        }
                    }}
                </Show>
                <p class="status">{move || otp_status.get()}</p>
            </div>
        </section>
    }
}
