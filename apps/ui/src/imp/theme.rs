//! Dark/light theme read + toggle, for the Quick Settings panel's theme
//! chip — the same `AppsUseLightTheme`/`SystemUsesLightTheme` registry
//! values the real Settings app's toggle writes, followed by the same
//! `WM_SETTINGCHANGE` broadcast Explorer sends so already-running apps
//! (this shell included) pick up the change live instead of needing a
//! restart.

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::System::Registry::{
    RegCloseKey, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER,
    KEY_READ, KEY_WRITE, REG_DWORD,
};
use windows::Win32::UI::WindowsAndMessaging::{
    SendMessageTimeoutW, HWND_BROADCAST, SMTO_ABORTIFHUNG, WM_SETTINGCHANGE,
};

const PERSONALIZE_KEY: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
const APPS_LIGHT_VALUE: PCWSTR = w!("AppsUseLightTheme");
const SYSTEM_LIGHT_VALUE: PCWSTR = w!("SystemUsesLightTheme");

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

fn write_dword(key: HKEY, value_name: PCWSTR, data: u32) -> windows::core::Result<()> {
    // SAFETY: `key` is a live, open registry key for the duration of
    // this call; `data` is a plain local read only for the call's
    // duration.
    unsafe {
        let bytes = data.to_ne_bytes();
        RegSetValueExW(key, value_name, 0, REG_DWORD, Some(&bytes)).ok()
    }
}

/// `None` when the key doesn't exist yet (pre-Windows-10-1809 systems,
/// or a fresh account that's never touched personalization) — the theme
/// chip hides itself in that case rather than guessing.
pub(crate) fn apps_use_light_theme() -> Option<bool> {
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

/// Flips both the apps and system (taskbar/Start) theme together —
/// Settings' single "Dark mode" toggle does the same, even though
/// they're two separate values.
pub(crate) fn set_apps_use_light_theme(light: bool) {
    let mut key = HKEY::default();
    // SAFETY: opened with write access; closed before returning on
    // every path, including the early return below.
    let opened = unsafe {
        RegOpenKeyExW(HKEY_CURRENT_USER, PERSONALIZE_KEY, 0, KEY_READ | KEY_WRITE, &mut key)
            .is_ok()
    };
    if !opened {
        return;
    }
    let value = u32::from(light);
    let _ = write_dword(key, APPS_LIGHT_VALUE, value);
    let _ = write_dword(key, SYSTEM_LIGHT_VALUE, value);
    // SAFETY: `key` was successfully opened above.
    unsafe {
        let _ = RegCloseKey(key);
    }

    // SAFETY: a broadcast `SendMessageTimeoutW` with no output pointer
    // requested has no further preconditions; `w!("ImmersiveColorSet")`
    // is a `'static` wide string literal, valid for the call's duration.
    unsafe {
        let _ = SendMessageTimeoutW(
            HWND_BROADCAST,
            WM_SETTINGCHANGE,
            WPARAM(0),
            LPARAM(w!("ImmersiveColorSet").as_ptr() as isize),
            SMTO_ABORTIFHUNG,
            200,
            None,
        );
    }
}

/// Convenience for the toggle chip's click handler.
pub(crate) fn toggle_theme() {
    if let Some(light) = apps_use_light_theme() {
        set_apps_use_light_theme(!light);
    }
}
