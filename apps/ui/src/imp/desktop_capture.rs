//! A picture of the desktop with no windows on it: wallpaper and icons,
//! exactly as Explorer draws them.
//!
//! The overview's first frame has to be indistinguishable from the
//! desktop, because nothing fades in over it: the frame is opaque from
//! the start and the transition is its parts moving. The workspace card
//! starts as the whole monitor, so it needs the very pixels that are
//! behind the windows. Drawing the wallpaper file ourselves does not
//! give those: the fit (fill, fit, span, a different crop per monitor)
//! is Explorer's business, and the icons are not in the file at all.
//!
//! `PrintWindow` on `Progman` does give them, for every monitor at once,
//! whatever is stacked on top. It takes tens of milliseconds, and this
//! thread also runs the low-level input hooks, so the capture is taken
//! on a worker thread and handed back with a message. It is refreshed
//! when the overview closes, which is the moment nobody is looking at
//! it; an icon moved since then shows in its old place until the next
//! refresh.
//!
//! The pixels are kept only until every overview has uploaded them.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::ffi::c_void;
use std::rc::Rc;

use windows::core::w;
use windows::Win32::Foundation::{HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GdiFlush, GetDC, ReleaseDC,
    SelectObject, BITMAPINFO, BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS,
};
use windows::Win32::Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS};
use windows::Win32::UI::HiDpi::{
    SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
};
use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, GetWindowRect, PostMessageW};

/// `PW_RENDERFULLCONTENT`: composed content, not just what GDI drew.
const PW_RENDERFULLCONTENT: PRINT_WINDOW_FLAGS = PRINT_WINDOW_FLAGS(2);

/// One monitor's part of a capture: tightly packed, opaque BGRA.
pub(crate) struct Capture {
    pub(crate) width: i32,
    pub(crate) height: i32,
    pub(crate) pixels: Rc<Vec<u8>>,
}

/// What the worker sends back: device name, size, pixels.
type Captured = Vec<(String, i32, i32, Vec<u8>)>;

thread_local! {
    static CAPTURES: RefCell<HashMap<String, Capture>> = RefCell::new(HashMap::new());
    /// Bumped for every capture that arrives. A surface remembers which
    /// one it was painted from, so it is uploaded once per capture.
    static GENERATION: Cell<u64> = const { Cell::new(0) };
    static IN_FLIGHT: Cell<bool> = const { Cell::new(false) };
}

/// The newest capture's number, or 0 if none has arrived yet.
pub(crate) fn generation() -> u64 {
    GENERATION.with(Cell::get)
}

/// The newest capture of `monitor`, if its pixels are still held and it
/// is `width` by `height`. A size mismatch means the monitor changed
/// under the capture, which then shows the wrong thing entirely.
pub(crate) fn pixels(monitor: &str, width: i32, height: i32) -> Option<Rc<Vec<u8>>> {
    CAPTURES.with(|c| {
        c.borrow()
            .get(monitor)
            .filter(|cap| cap.width == width && cap.height == height)
            .map(|cap| cap.pixels.clone())
    })
}

/// Lets go of the pixels once they have been uploaded.
pub(crate) fn release_pixels() {
    CAPTURES.with(|c| c.borrow_mut().clear());
}

/// Starts a capture, unless one is already on its way. The result
/// arrives at `target` as `message`, to be handed to [`on_captured`].
pub(crate) fn request(target: HWND, message: u32) {
    if IN_FLIGHT.with(|f| f.replace(true)) {
        return;
    }
    let monitors: Vec<(String, RECT)> = super::monitors::enumerate_monitors()
        .into_iter()
        .map(|m| (m.device_name, m.rect))
        .collect();
    let target = target.0 as isize;
    let spawned = std::thread::Builder::new().name("desktop-capture".into()).spawn(move || {
        let captured: Captured = capture(&monitors).unwrap_or_default();
        let boxed = Box::into_raw(Box::new(captured));
        // SAFETY: `target` is the primary bar, a process-lifetime
        // window. Ownership of `boxed` passes to the receiver; if the
        // post fails it is taken back here.
        unsafe {
            if PostMessageW(HWND(target as *mut c_void), message, WPARAM(0), LPARAM(boxed as isize))
                .is_err()
            {
                drop(Box::from_raw(boxed));
            }
        }
    });
    if spawned.is_err() {
        IN_FLIGHT.with(|f| f.set(false));
    }
}

/// Takes delivery of a capture posted by [`request`]. Returns whether
/// there is anything new to paint.
pub(crate) fn on_captured(lparam: LPARAM) -> bool {
    IN_FLIGHT.with(|f| f.set(false));
    if lparam.0 == 0 {
        return false;
    }
    // SAFETY: the pointer was made by `Box::into_raw` in `request` and
    // is delivered exactly once.
    let captured: Captured = *unsafe { Box::from_raw(lparam.0 as *mut Captured) };
    if captured.is_empty() {
        return false;
    }
    CAPTURES.with(|c| {
        let mut map = c.borrow_mut();
        map.clear();
        for (name, width, height, pixels) in captured {
            map.insert(name, Capture { width, height, pixels: Rc::new(pixels) });
        }
    });
    GENERATION.with(|g| g.set(g.get() + 1));
    true
}

/// Runs on the worker thread.
fn capture(monitors: &[(String, RECT)]) -> Option<Captured> {
    // SAFETY: every handle below is created and released within this
    // call, on this thread; `bits` is valid while `bitmap` is alive.
    unsafe {
        // Rects must be physical pixels, the same space the UI thread
        // enumerated the monitors in.
        let _ = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);

        let progman = FindWindowW(w!("Progman"), None).ok()?;
        let mut desk = RECT::default();
        GetWindowRect(progman, &mut desk).ok()?;
        let (width, height) = (desk.right - desk.left, desk.bottom - desk.top);
        if width <= 0 || height <= 0 {
            return None;
        }

        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: core::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height, // top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0 as u32,
                ..Default::default()
            },
            ..Default::default()
        };
        let screen = GetDC(None);
        let mem = CreateCompatibleDC(screen);
        let mut bits: *mut c_void = std::ptr::null_mut();
        let bitmap = CreateDIBSection(screen, &info, DIB_RGB_COLORS, &mut bits, None, 0).ok();
        ReleaseDC(None, screen);
        let Some(bitmap) = bitmap.filter(|_| !bits.is_null()) else {
            let _ = DeleteDC(mem);
            return None;
        };

        let previous = SelectObject(mem, bitmap);
        let ok = PrintWindow(progman, mem, PW_RENDERFULLCONTENT).as_bool();
        let _ = GdiFlush();
        SelectObject(mem, previous);
        let _ = DeleteDC(mem);

        let mut out = Vec::new();
        if ok {
            let all = std::slice::from_raw_parts(bits as *const u8, width as usize * height as usize * 4);
            for (name, rect) in monitors {
                if let Some(pixels) = crop(all, width, height, desk, *rect) {
                    out.push((name.clone(), rect.right - rect.left, rect.bottom - rect.top, pixels));
                }
            }
        }
        let _ = DeleteObject(bitmap);
        (!out.is_empty()).then_some(out)
    }
}

/// Cuts `rect` (screen coordinates) out of a top-down BGRA buffer that
/// covers `desk`, and makes it opaque: GDI leaves the alpha byte at
/// whatever it was. `None` if `rect` is not wholly inside `desk`.
fn crop(all: &[u8], width: i32, height: i32, desk: RECT, rect: RECT) -> Option<Vec<u8>> {
    let (x, y) = (rect.left - desk.left, rect.top - desk.top);
    let (w, h) = (rect.right - rect.left, rect.bottom - rect.top);
    if w <= 0 || h <= 0 || x < 0 || y < 0 || x + w > width || y + h > height {
        return None;
    }
    if all.len() < width as usize * height as usize * 4 {
        return None;
    }
    let mut out = Vec::with_capacity(w as usize * h as usize * 4);
    for row in 0..h as usize {
        let start = ((y as usize + row) * width as usize + x as usize) * 4;
        out.extend_from_slice(&all[start..start + w as usize * 4]);
    }
    for px in out.chunks_exact_mut(4) {
        px[3] = 0xFF;
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 4x2 desktop whose pixels are numbered, so a crop can be read.
    fn desk() -> (Vec<u8>, RECT) {
        let mut all = Vec::new();
        for i in 0..8u8 {
            all.extend_from_slice(&[i, i, i, 0]);
        }
        (all, RECT { left: -2, top: 0, right: 2, bottom: 2 })
    }

    #[test]
    fn a_monitor_left_of_the_primary_is_cut_from_the_left_of_the_capture() {
        let (all, desk) = desk();
        let left = crop(&all, 4, 2, desk, RECT { left: -2, top: 0, right: 0, bottom: 2 }).unwrap();
        let firsts: Vec<u8> = left.chunks_exact(4).map(|p| p[0]).collect();
        assert_eq!(firsts, vec![0, 1, 4, 5]);
        let right = crop(&all, 4, 2, desk, RECT { left: 0, top: 0, right: 2, bottom: 2 }).unwrap();
        let firsts: Vec<u8> = right.chunks_exact(4).map(|p| p[0]).collect();
        assert_eq!(firsts, vec![2, 3, 6, 7]);
    }

    #[test]
    fn the_crop_is_made_opaque() {
        let (all, desk) = desk();
        let out = crop(&all, 4, 2, desk, RECT { left: -2, top: 0, right: 2, bottom: 2 }).unwrap();
        assert!(out.chunks_exact(4).all(|p| p[3] == 0xFF));
    }

    #[test]
    fn a_monitor_outside_the_capture_is_refused_rather_than_read_out_of_bounds() {
        let (all, desk) = desk();
        assert!(crop(&all, 4, 2, desk, RECT { left: 1, top: 0, right: 3, bottom: 2 }).is_none());
        assert!(crop(&all, 4, 2, desk, RECT { left: -2, top: 1, right: 0, bottom: 3 }).is_none());
        assert!(crop(&all, 4, 2, desk, RECT { left: 0, top: 0, right: 0, bottom: 2 }).is_none());
        assert!(crop(&all[..8], 4, 2, desk, RECT { left: -2, top: 0, right: 0, bottom: 2 }).is_none());
    }
}
