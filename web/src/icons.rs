// Pictogram-only buttons for the small, repeated per-item actions (trash,
// rename, restore) where a text label would be more visual clutter than
// help — a text button stays right for page-level actions (Create album,
// Log in), where the label *is* the point. Each glyph is paired with a
// real accessible name via the button's own `aria-label`/`title`, never
// carried by the glyph alone: emoji have inconsistent (or no) names
// announced by screen readers on their own.

use leptos::prelude::*;

pub const TRASH: &str = "🗑️";
pub const PENCIL: &str = "✏️";
pub const RESTORE: &str = "↩️";

/// An icon glyph marked `aria-hidden` — the button wrapping it carries the
/// actual accessible name (`aria-label`, plus `title` for a mouse-hover
/// tooltip), so assistive tech doesn't also try to announce the raw emoji.
pub fn icon(glyph: &str) -> impl IntoView {
    let glyph = glyph.to_string();
    view! { <span aria-hidden="true">{glyph}</span> }
}
