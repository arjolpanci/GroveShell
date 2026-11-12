//! Which *app* a window belongs to, for windows whose process says
//! nothing useful.
//!
//! Every UWP app's top-level window is an `ApplicationFrameWindow` owned
//! by `ApplicationFrameHost.exe`: Settings, Calculator and the rest all
//! report the same executable, and the frame answers neither
//! `WM_GETICON` nor `GCLP_HICON`. Grouping by exe therefore folded them
//! into one dock entry with no icon at all — a gap with dots under it.
//!
//! The shell's answer is the window's AppUserModelID, which the frame
//! does carry, and which names an item in `shell:AppsFolder` that can
//! render the app's real icon.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::c_void;

use windows::core::{BSTR, HSTRING, PROPVARIANT};
use windows::Win32::Foundation::{HWND, SIZE};
use windows::Win32::Graphics::Gdi::{
    CreateBitmap, DeleteObject, GetObjectW, BITMAP, HBITMAP,
};
use windows::Win32::Storage::EnhancedStorage::PKEY_AppUserModel_ID;
use windows::Win32::UI::Shell::PropertiesSystem::{IPropertyStore, SHGetPropertyStoreForWindow};
use windows::Win32::UI::Shell::{
    IShellItemImageFactory, SHCreateItemFromParsingName, SIIGBF_BIGGERSIZEOK, SIIGBF_ICONONLY,
};
use windows::Win32::UI::WindowsAndMessaging::{CreateIconIndirect, HICON, ICONINFO};

/// Window class of the frame that hosts every UWP app's view.
pub(crate) const UWP_FRAME_CLASS: &str = "ApplicationFrameWindow";

/// Edge of the bitmap requested from the shell; the dock scales it down.
const ICON_PX: i32 = 256;

thread_local! {
    /// AUMID → icon. Ours to free, but kept for the process's lifetime
    /// like `dock::PINNED_ICON_CACHE`: the set is small and an app's
    /// icon does not change while the shell runs. A failed lookup is
    /// cached too, so a broken package is asked once, not per rebuild.
    static AUMID_ICONS: RefCell<HashMap<String, Option<isize>>> = RefCell::new(HashMap::new());
}

/// The AppUserModelID the shell has on record for `hwnd`, if any.
pub(crate) fn window_aumid(hwnd: HWND) -> Option<String> {
    // SAFETY: `hwnd` is a top-level window handle; a stale one makes the
    // call fail rather than misbehave. The store and the variant are
    // released by their wrappers.
    unsafe {
        let store: IPropertyStore = SHGetPropertyStoreForWindow(hwnd).ok()?;
        let value: PROPVARIANT = store.GetValue(&PKEY_AppUserModel_ID).ok()?;
        let id = BSTR::try_from(&value).ok()?.to_string();
        (!id.is_empty()).then_some(id)
    }
}

/// The key windows are grouped under in the dock: the AUMID for a UWP
/// frame, the lowercased executable name for everything else.
pub(crate) fn group_key(class: &str, exe_name: Option<&str>, hwnd: isize) -> Option<String> {
    if class == UWP_FRAME_CLASS {
        if let Some(id) = window_aumid(HWND(hwnd as *mut c_void)) {
            return Some(id.to_lowercase());
        }
    }
    exe_name.map(str::to_lowercase)
}

/// The app's icon as the Start menu shows it, for a UWP frame window.
/// `None` for any other window, or when the shell has nothing.
pub(crate) fn uwp_window_icon(hwnd: HWND) -> Option<HICON> {
    let aumid = window_aumid(hwnd)?;
    if let Some(cached) = AUMID_ICONS.with(|c| c.borrow().get(&aumid).copied()) {
        return cached.map(|h| HICON(h as *mut c_void));
    }
    let icon = aumid_icon(&aumid);
    AUMID_ICONS.with(|c| c.borrow_mut().insert(aumid, icon.map(|h| h.0 as isize)));
    icon
}

fn aumid_icon(aumid: &str) -> Option<HICON> {
    let path = HSTRING::from(format!(r"shell:AppsFolder\{aumid}"));
    // SAFETY: `path` outlives the call. `GetImage` hands back a bitmap
    // this process owns, freed below once the icon has copied it.
    unsafe {
        let factory: IShellItemImageFactory = SHCreateItemFromParsingName(&path, None).ok()?;
        let size = SIZE { cx: ICON_PX, cy: ICON_PX };
        let color = factory.GetImage(size, SIIGBF_ICONONLY | SIIGBF_BIGGERSIZEOK).ok()?;
        let icon = bitmap_to_icon(color);
        let _ = DeleteObject(color);
        icon
    }
}

/// Wraps a 32-bit alpha bitmap as an icon.
///
/// The shell returns premultiplied alpha; an icon's colour bitmap is
/// read as straight alpha. Left alone, every soft edge would be blended
/// twice and come out as a dark fringe, so the pixels are un-multiplied
/// in place first.
///
/// # Safety
/// `color` must be a valid bitmap handle owned by the caller.
unsafe fn bitmap_to_icon(color: HBITMAP) -> Option<HICON> {
    let mut bm = BITMAP::default();
    let got = GetObjectW(color, std::mem::size_of::<BITMAP>() as i32, Some(&mut bm as *mut _ as *mut c_void));
    if got == 0 || bm.bmWidth <= 0 || bm.bmHeight <= 0 {
        return None;
    }
    // `bmBits` is only set for a DIB section, which is what the shell
    // returns; anything else is wrapped as it is.
    if bm.bmBitsPixel == 32 && !bm.bmBits.is_null() {
        let len = bm.bmWidth as usize * bm.bmHeight as usize * 4;
        let pixels = std::slice::from_raw_parts_mut(bm.bmBits as *mut u8, len);
        unpremultiply(pixels);
    }
    // With a 32-bit colour bitmap the mask is ignored, but it must exist
    // and match in size.
    let mask = CreateBitmap(bm.bmWidth, bm.bmHeight, 1, 1, None);
    if mask.is_invalid() {
        return None;
    }
    let info = ICONINFO {
        fIcon: true.into(),
        xHotspot: 0,
        yHotspot: 0,
        hbmMask: mask,
        hbmColor: color,
    };
    let icon = CreateIconIndirect(&info).ok();
    let _ = DeleteObject(mask);
    icon.filter(|i| !i.is_invalid())
}

/// Converts BGRA pixels from premultiplied to straight alpha, in place.
fn unpremultiply(pixels: &mut [u8]) {
    for px in pixels.chunks_exact_mut(4) {
        let a = px[3] as u32;
        if a == 0 || a == 255 {
            continue;
        }
        for c in &mut px[..3] {
            *c = ((*c as u32 * 255 + a / 2) / a).min(255) as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unpremultiply_restores_the_straight_colour() {
        // Half-transparent pure red, premultiplied: (0, 0, 128, 128).
        let mut px = [0u8, 0, 128, 128];
        unpremultiply(&mut px);
        assert_eq!(px, [0, 0, 255, 128]);
    }

    #[test]
    fn unpremultiply_leaves_opaque_and_empty_pixels_alone() {
        let mut px = [10u8, 20, 30, 255, 0, 0, 0, 0];
        unpremultiply(&mut px);
        assert_eq!(px, [10, 20, 30, 255, 0, 0, 0, 0]);
    }

    #[test]
    fn group_key_falls_back_to_the_exe_for_ordinary_windows() {
        assert_eq!(group_key("Notepad", Some("Notepad.EXE"), 0), Some("notepad.exe".to_string()));
        assert_eq!(group_key("Notepad", None, 0), None);
    }
}
