//! Reading Windows' light/dark apps preference — the
//! `AppsUseLightTheme` value the real Settings app's "Dark mode" toggle
//! writes.
//!
//! The kit owns the *read* because `design::color::refresh_theme` needs
//! it to resolve every token; writing the value back (and broadcasting
//! `WM_SETTINGCHANGE` afterwards) stays in `apps/ui`, which is the only
//! thing that offers a theme toggle.

use windows::core::{w, PCWSTR};
use windows::Win32::System::Registry::{
    RegCloseKey, RegOpenKeyExW, RegQueryValueExW, HKEY, HKEY_CURRENT_USER, KEY_READ,
};

const PERSONALIZE_KEY: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
const APPS_LIGHT_VALUE: PCWSTR = w!("AppsUseLightTheme");

fn read_dword(key: HKEY, value_name: PCWSTR) -> Option<u32> {
    // SAFETY: `key` is a live, open registry key for the duration of
    // this call; `buf` is sized exactly for a `REG_DWORD` and `size` is
    // set to that size before the call, matching what
    // `RegQueryValueExW` requires.
    unsafe {
        let mut buf = 0u32;
        let mut size = std::mem::size_of::<u32>() as u32;
        RegQueryValueExW(
            key,
            value_name,
            None,
            None,
            Some(&mut buf as *mut u32 as *mut u8),
            Some(&mut size),
        )
        .ok()
        .ok()?;
        Some(buf)
    }
}

/// `None` when the key doesn't exist yet (pre-Windows-10-1809 systems,
/// or a fresh account that's never touched personalization) — the theme
/// chip hides itself in that case rather than guessing.
pub fn apps_use_light_theme() -> Option<bool> {
    let mut key = HKEY::default();
    // SAFETY: `key` is written by this call and only read afterward;
    // closed before returning on every path.
    unsafe {
        RegOpenKeyExW(HKEY_CURRENT_USER, PERSONALIZE_KEY, 0, KEY_READ, &mut key).ok().ok()?;
    }
    let value = read_dword(key, APPS_LIGHT_VALUE);
    // SAFETY: `key` was successfully opened above.
    unsafe {
        let _ = RegCloseKey(key);
    }
    value.map(|v| v != 0)
}
