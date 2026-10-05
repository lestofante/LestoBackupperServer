// The underlined-letter mnemonic convention classic desktop toolkits
// (Windows, GTK, Qt) use to mark a control's Alt+letter access key —
// applied here to every action button, paired with the HTML `accesskey`
// attribute so it's a real shortcut, not just a visual hint. Browsers
// don't render `accesskey` with an underline themselves, hence spelling
// it out in markup instead of relying on the attribute alone.

use leptos::prelude::*;

/// Wraps the first case-insensitive occurrence of `key` in `label` with a
/// `<span class="mnemonic">` for the underline. Pass `key` again to
/// `attr:accesskey` on the same element to make it a real shortcut — kept
/// separate rather than bundled into one component because call sites
/// need very different element types (`<button>`, `<a>`, a checkbox's
/// `<label>`) and closure signatures for `on:click`.
pub fn mnemonic(label: &str, key: char) -> impl IntoView {
    match label.find(|c: char| c.eq_ignore_ascii_case(&key)) {
        Some(idx) => {
            let letter_len = label[idx..].chars().next().map(char::len_utf8).unwrap_or(1);
            let before = label[..idx].to_string();
            let letter = label[idx..idx + letter_len].to_string();
            let after = label[idx + letter_len..].to_string();
            view! { <span>{before}<span class="mnemonic">{letter}</span>{after}</span> }.into_any()
        }
        None => view! { <span>{label.to_string()}</span> }.into_any(),
    }
}
