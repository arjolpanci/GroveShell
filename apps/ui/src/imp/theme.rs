//! Dark/light theme read + toggle, for the Quick Settings panel's theme
//! chip — the same `AppsUseLightTheme`/`SystemUsesLightTheme` registry
//! values the real Settings app's toggle writes, followed by the same
//! `WM_SETTINGCHANGE` broadcast Explorer sends so already-running apps
//! (this shell included) pick up the change live instead of needing a
//! restart.

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::System::Registry::{
    RegCloseKey, RegOpenKeyExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_WRITE,
    REG_DWORD,
};
use windows::Win32::UI::WindowsAndMessaging::{
    SendMessageTimeoutW, HWND_BROADCAST, SMTO_ABORTIFHUNG, WM_SETTINGCHANGE,
};

const PERSONALIZE_KEY: PCWSTR = w!(r"Software\Microsoft\Windows\CurrentVersion\Themes\Personalize");
const APPS_LIGHT_VALUE: PCWSTR = w!("AppsUseLightTheme");
const SYSTEM_LIGHT_VALUE: PCWSTR = w!("SystemUsesLightTheme");

fn write_dword(key: HKEY, value_name: PCWSTR, data: u32) -> windows::core::Result<()> {
    // SAFETY: `key` is a live, open registry key for the duration of
    // this call; `data` is a plain local read only for the call's
    // duration.
    unsafe {
        let bytes = data.to_ne_bytes();
        RegSetValueExW(key, value_name, 0, REG_DWORD, Some(&bytes)).ok()
    }
}

/// The read lives in the UI kit now, which needs it to resolve every
/// design token; this binary keeps the write and the broadcast.
pub(crate) use groveshell_ui_kit::theme::apps_use_light_theme;

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
