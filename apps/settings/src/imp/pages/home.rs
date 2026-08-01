//! Home/Status page — full implementation in Task 9.

use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Gdi::HDC;

use super::Page;

pub(crate) struct HomePage;

impl HomePage {
    pub(crate) fn new() -> Self {
        Self
    }
}

impl Page for HomePage {
    fn paint(&self, _hdc: HDC, _content_rect: RECT) {}
    fn on_click(&mut self, _x: i32, _y: i32, _content_rect: RECT) {}
}
