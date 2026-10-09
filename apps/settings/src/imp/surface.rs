//! The settings window's drawing surface.
//!
//! Direct2D on a DirectComposition surface when the GPU path is
//! available, so the window can leave its background transparent and let
//! DWM's Mica show through; plain GDI into a back buffer otherwise. The
//! window is written once against `Canvas` and does not know which one it
//! got — ADR-010's "one painter, two canvas backends", now that both
//! backends live in the shared kit.

use groveshell_ui_kit::canvas::{Canvas, D2DCanvas, GdiCanvas};
use groveshell_ui_kit::gpu::{self, GpuSurface};
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, EndPaint,
    SelectObject, BitBlt, PAINTSTRUCT, SRCCOPY,
};

/// Creates the window's composition surface, or `None` when the GPU path
/// is unavailable and the caller should fall back to GDI.
pub(crate) fn create(hwnd: HWND, width: i32, height: i32) -> Option<GpuSurface> {
    gpu::init();
    gpu::create_surface(hwnd, width.max(1), height.max(1))
}

/// Paints one frame through whichever backend is live.
///
/// `draw` receives a canvas and the client rect. On the Direct2D path the
/// surface is cleared to alpha 0 first: anything not drawn stays fully
/// transparent, which is what lets the Mica backdrop appear at all.
pub(crate) fn paint<F>(hwnd: HWND, surface: Option<&GpuSurface>, client: RECT, dpi: u32, draw: F)
where
    F: FnOnce(&mut dyn Canvas, RECT),
{
    if let Some(surface) = surface {
        gpu::redraw(surface, |ctx| {
            gpu::clear_transparent(ctx);
            let mut canvas = D2DCanvas::new(ctx, dpi);
            draw(&mut canvas, client);
        });
        gpu::commit();
        // The window still gets a WM_PAINT it must answer, or Windows
        // keeps the update region dirty and sends it again forever.
        // SAFETY: `hwnd` is the window currently handling `WM_PAINT`.
        unsafe {
            let mut ps = PAINTSTRUCT::default();
            let _ = BeginPaint(hwnd, &mut ps);
            let _ = EndPaint(hwnd, &ps);
        }
        return;
    }

    // GDI fallback: draw into a back buffer and blit once, so the window
    // never shows a half-painted frame.
    // SAFETY: `hwnd` is the window currently handling `WM_PAINT`; every
    // GDI object created here is selected out and deleted before return.
    unsafe {
        let mut ps = PAINTSTRUCT::default();
        let window_dc = BeginPaint(hwnd, &mut ps);
        let buffer = CreateCompatibleDC(window_dc);
        let bitmap = CreateCompatibleBitmap(window_dc, client.right, client.bottom);
        let previous = SelectObject(buffer, bitmap);

        let mut canvas = GdiCanvas::new(buffer, dpi);
        draw(&mut canvas, client);

        let _ = BitBlt(window_dc, 0, 0, client.right, client.bottom, buffer, 0, 0, SRCCOPY);
        SelectObject(buffer, previous);
        let _ = DeleteObject(bitmap);
        let _ = DeleteDC(buffer);
        let _ = EndPaint(hwnd, &ps);
    }
}
