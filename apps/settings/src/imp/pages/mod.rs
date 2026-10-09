//! `Page` implemented by each settings screen.
//!
//! A page is a list of cards. Layout, painting, hit-testing and keyboard
//! traversal all come from `groveshell_ui_kit::rows`, so a page never
//! computes a rectangle and the four can never disagree — the rule the
//! shell already follows for its own bar regions.

use groveshell_ui_kit::rows::Card;

pub(crate) mod accessibility;
pub(crate) mod dock;
pub(crate) mod home;
pub(crate) mod input;
pub(crate) mod overview;
pub(crate) mod top_bar;

pub(crate) trait Page {
    /// This page's content, rebuilt from the current config on each
    /// paint, so a change made anywhere shows up here without this page
    /// caching or invalidating anything.
    fn cards(&self) -> Vec<Card>;

    /// A row was clicked, or activated from the keyboard.
    fn on_activate(&mut self, id: u32);

    /// A slider or choice row produced a new value. `value` is the
    /// slider's position for a slider, and the chosen index for a choice.
    fn on_value(&mut self, id: u32, value: f32) {
        let _ = (id, value);
    }
}

/// Picks the index of `value` in `options`, falling back to `default`
/// when the config holds something the UI doesn't offer (a hand-edited
/// `config.toml`, or a value from a newer build).
pub(crate) fn option_index(options: &[&str], value: &str, default: usize) -> usize {
    options.iter().position(|o| *o == value).unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn option_index_finds_a_known_value() {
        assert_eq!(option_index(&["left", "center", "right"], "right", 0), 2);
    }

    #[test]
    fn option_index_falls_back_for_an_unknown_value() {
        assert_eq!(option_index(&["left", "center", "right"], "sideways", 1), 1);
    }
}
