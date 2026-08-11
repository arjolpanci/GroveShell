//! `Page` trait implemented by each settings screen (Home, Dock, Top Bar,
//! Overview, Input) — see Tasks 9, 15, 16, 17, 18.

use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Gdi::HDC;

pub(crate) mod accessibility;
pub(crate) mod dock;
pub(crate) mod home;
pub(crate) mod input;
pub(crate) mod overview;
pub(crate) mod top_bar;

pub(crate) trait Page {
    /// Paints this page's content into `content_rect` (already excludes
    /// the nav list — the area to the right of it).
    fn paint(&self, hdc: HDC, content_rect: RECT);
    /// Handles a left-click at window-client-relative `(x, y)`, given the
    /// same `content_rect` `paint` was last called with.
    fn on_click(&mut self, x: i32, y: i32, content_rect: RECT);
}
