// Dolphin-style keyboard navigation + multi-select for a tile grid
// (Library, Albums, an album's file grid): arrow keys move a focus ring,
// Space toggles the focused tile's selection, Shift+arrow extends a
// range, Ctrl/Cmd+A selects everything, Escape clears. Mouse: Ctrl/Cmd+
// click toggles one tile, Shift+click range-selects — a plain click keeps
// this app's existing "click a tile to open it" behavior untouched.
//
// Entirely gated by `AppContext::keyboard_shortcuts` (a Settings toggle)
// and backs off whenever `browser::is_editable_target()` is true, so it
// never competes with typing in a text field or the browser's own
// shortcuts — see that function's doc comment.

use std::collections::HashSet;

use leptos::prelude::*;

use crate::browser;

#[derive(Clone, Copy)]
pub struct GridSelection {
    pub focused: RwSignal<Option<usize>>,
    pub selected: RwSignal<HashSet<String>>,
    pub anchor: RwSignal<Option<usize>>,
}

impl GridSelection {
    pub fn new() -> Self {
        Self { focused: RwSignal::new(None), selected: RwSignal::new(HashSet::new()), anchor: RwSignal::new(None) }
    }

    pub fn is_selected(&self, id: &str) -> bool {
        self.selected.get().contains(id)
    }

    pub fn selected_count(&self) -> usize {
        self.selected.get().len()
    }

    /// Avoids a bare `>` inside `view!`'s prop-value parser, which the
    /// `when=move || grid.selected_count() > 0` form trips over.
    pub fn has_selection(&self) -> bool {
        !self.selected.get().is_empty()
    }

    pub fn clear_selection(&self) {
        self.selected.update(|s| s.clear());
    }

    pub fn toggle(&self, id: &str) {
        self.selected.update(|s| {
            if !s.remove(id) {
                s.insert(id.to_string());
            }
        });
    }

    fn select_range(&self, ids: &[String], from: usize, to: usize) {
        let (lo, hi) = if from <= to { (from, to) } else { (to, from) };
        if let Some(slice) = ids.get(lo..=hi.min(ids.len().saturating_sub(1))) {
            self.selected.update(|s| s.extend(slice.iter().cloned()));
        }
    }

    pub fn select_all(&self, ids: &[String]) {
        self.selected.set(ids.iter().cloned().collect());
    }

    /// The id the focus ring currently sits on, re-derived from `ids` so
    /// it stays correct if the list changes shape (an item removed, etc).
    pub fn focused_id(&self, ids: &[String]) -> Option<String> {
        self.focused.get().and_then(|i| ids.get(i).cloned())
    }

    /// Wires up mouse-driven selection for one tile at `index`. Returns
    /// `true` when the click was consumed for selection (caller should
    /// `prevent_default()` on the wrapping link so it doesn't also
    /// navigate); `false` means it was a plain click and the caller should
    /// let normal navigation happen.
    pub fn handle_tile_click(&self, ev: &leptos::ev::MouseEvent, ids: &[String], index: usize) -> bool {
        if ev.shift_key() {
            let anchor = self.anchor.get().unwrap_or(index);
            self.select_range(ids, anchor, index);
            self.focused.set(Some(index));
            return true;
        }
        if ev.ctrl_key() || ev.meta_key() {
            if let Some(id) = ids.get(index) {
                self.toggle(id);
            }
            self.anchor.set(Some(index));
            self.focused.set(Some(index));
            return true;
        }
        false
    }

    /// Registers the window-level keydown handler for this grid. Call
    /// once per component (not inside an `Effect`) — cleaned up
    /// automatically via `on_cleanup` when the component unmounts.
    ///
    /// `ids` is re-read on every keypress (not cached), so it always
    /// reflects the grid's current contents. `grid_selector` is a CSS
    /// selector matching one tile, used only to measure how many sit per
    /// row (`browser::items_per_row`) so ArrowUp/Down move a full row.
    /// `on_open` fires on Enter for the focused tile. `on_escape_up` fires
    /// on Escape only when there's no selection to clear first — Escape
    /// with an active selection just clears it, matching a file manager's
    /// "first Escape deselects, second goes up" convention; pass a no-op
    /// for a top-level grid with no parent to go up to.
    pub fn bind_keyboard(
        &self,
        kb_enabled: RwSignal<bool>,
        ids: impl Fn() -> Vec<String> + 'static,
        grid_selector: &'static str,
        on_open: impl Fn(String) + 'static,
        on_escape_up: impl Fn() + 'static,
    ) {
        let selection = *self;
        let handle = window_event_listener(leptos::ev::keydown, move |ev| {
            if !kb_enabled.get_untracked() || browser::is_editable_target() {
                return;
            }
            // Checked before the empty-grid bail-out below: "go up" makes
            // sense even for a grid with nothing in it (an empty album),
            // and doesn't need `items` at all.
            if ev.key() == "Escape" {
                if selection.has_selection() {
                    selection.clear_selection();
                } else {
                    on_escape_up();
                }
                return;
            }
            let items = ids();
            if items.is_empty() {
                return;
            }
            let len = items.len();
            let per_row = browser::items_per_row(grid_selector).max(1);
            let current = selection.focused.get_untracked();

            let move_to = |new_index: usize, extend: bool| {
                let new_index = new_index.min(len - 1);
                if extend {
                    let anchor = selection.anchor.get_untracked().unwrap_or(current.unwrap_or(0));
                    selection.anchor.set(Some(anchor));
                    selection.select_range(&items, anchor, new_index);
                }
                selection.focused.set(Some(new_index));
            };
            // With nothing focused yet, any arrow key just brings the
            // ring onto the first tile (index 0) rather than acting on a
            // phantom "current position" — otherwise the very first
            // ArrowRight press would skip straight to index 1.
            let step = |f: fn(usize, usize) -> usize, delta: usize| match current {
                Some(c) => f(c, delta),
                None => 0,
            };

            match ev.key().as_str() {
                "ArrowRight" => {
                    ev.prevent_default();
                    move_to(step(usize::saturating_add, 1), ev.shift_key());
                }
                "ArrowLeft" => {
                    ev.prevent_default();
                    move_to(step(usize::saturating_sub, 1), ev.shift_key());
                }
                "ArrowDown" => {
                    ev.prevent_default();
                    move_to(step(usize::saturating_add, per_row), ev.shift_key());
                }
                "ArrowUp" => {
                    ev.prevent_default();
                    move_to(step(usize::saturating_sub, per_row), ev.shift_key());
                }
                "Home" => {
                    ev.prevent_default();
                    move_to(0, ev.shift_key());
                }
                "End" => {
                    ev.prevent_default();
                    move_to(len - 1, ev.shift_key());
                }
                "Enter" => {
                    if let Some(i) = selection.focused.get_untracked() {
                        ev.prevent_default();
                        on_open(items[i].clone());
                    }
                }
                " " => {
                    if let Some(i) = selection.focused.get_untracked() {
                        ev.prevent_default();
                        selection.toggle(&items[i]);
                        selection.anchor.set(Some(i));
                    }
                }
                "a" | "A" if ev.ctrl_key() || ev.meta_key() => {
                    ev.prevent_default();
                    selection.select_all(&items);
                }
                _ => {}
            }
        });
        on_cleanup(move || handle.remove());
    }
}
