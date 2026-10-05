use leptos::prelude::*;
use leptos::task::spawn_local;

use crate::api::{self, PendingUser};
use crate::context::use_app;
use crate::shortcuts::mnemonic;

#[component]
pub fn AdminView() -> impl IntoView {
    let ctx = use_app();
    let pending: RwSignal<Vec<PendingUser>> = RwSignal::new(Vec::new());

    let refresh = move || {
        let Some(client) = ctx.client.get_untracked() else { return };
        let Some(token) = ctx.token.get_untracked() else { return };
        spawn_local(async move {
            match api::list_pending_users(&client, &token).await {
                Ok(users) => pending.set(users),
                Err(message) => ctx.status.set(format!("couldn't load pending users: {message}")),
            }
        });
    };
    Effect::new(move |_| refresh());

    let approve = move |user_id: String| {
        let Some(client) = ctx.client.get_untracked() else { return };
        let Some(token) = ctx.token.get_untracked() else { return };
        spawn_local(async move {
            match api::approve_user(&client, &token, &user_id).await {
                Ok(_) => refresh(),
                Err(message) => ctx.status.set(format!("couldn't approve: {message}")),
            }
        });
    };

    view! {
        <section class="admin">
            <h2>"Pending accounts"</h2>
            <Show when=move || pending.get().is_empty()>
                <p class="empty">"No pending accounts."</p>
            </Show>
            <ul class="pending-list">
                <For each=move || pending.get() key=|u| u.user_id.clone() let:user>
                    <li>
                        <span>{user.username.clone()}</span>
                        <button accesskey="p" on:click=move |_| approve(user.user_id.clone())>{mnemonic("Approve", 'p')}</button>
                    </li>
                </For>
            </ul>
        </section>
    }
}
