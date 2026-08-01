//! Reads/writes the `HKCU\...\Run` value that makes Windows launch
//! `groveshell-settings.exe` at login — the process that then launches
//! everything else (see Task 5), not `groveshell-host` directly.

use windows::core::{w, PCWSTR};
use windows::Win32::System::Registry::{
    RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER,
    KEY_READ, KEY_WRITE, REG_SZ,
};

const RUN_KEY_PATH: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
const VALUE_NAME: &str = "GroveShell";

fn open_run_key(access: u32) -> Option<HKEY> {
    let path: Vec<u16> = RUN_KEY_PATH.encode_utf16().chain(std::iter::once(0)).collect();
    let mut key = HKEY::default();
    // SAFETY: `path` is nul-terminated and outlives the call; `key` is a
    // local out-param outliving it too.
    let result = unsafe {
        RegOpenKeyExW(HKEY_CURRENT_USER, PCWSTR(path.as_ptr()), 0, windows::Win32::System::Registry::REG_SAM_FLAGS(access), &mut key)
    };
    result.is_ok().then_some(key)
}

/// `true` if the `GroveShell` value exists under the Run key (its content
/// isn't validated — a stale/incorrect path is still "enabled" for
/// display purposes, matching how the real Windows Settings app's
/// Startup Apps page also just checks presence).
pub fn is_enabled() -> bool {
    let Some(key) = open_run_key(KEY_READ.0) else { return false };
    let name: Vec<u16> = VALUE_NAME.encode_utf16().chain(std::iter::once(0)).collect();
    let mut buf = [0u16; 512];
    let mut buf_len = (buf.len() * 2) as u32;
    // SAFETY: `key` was just opened above with read access; `name` is
    // nul-terminated; `buf`/`buf_len` describe a live, correctly-sized
    // output buffer for the duration of this call.
    let result = unsafe {
        windows::Win32::System::Registry::RegQueryValueExW(
            key,
            PCWSTR(name.as_ptr()),
            None,
            None,
            Some(buf.as_mut_ptr() as *mut u8),
            Some(&mut buf_len),
        )
    };
    // SAFETY: `key` was opened by this function and is not used past
    // this point.
    unsafe { let _ = RegCloseKey(key); }
    result.is_ok()
}

/// Writes (or removes) the `GroveShell` Run value pointing at this
/// process's own `.exe` path — always `groveshell-settings.exe`, per the
/// design decision that it's the process a user wants launched at login.
pub fn set_enabled(enabled: bool) {
    let Some(key) = open_run_key((KEY_READ | KEY_WRITE).0) else {
        tracing::warn!("could not open Run registry key");
        return;
    };
    let name: Vec<u16> = VALUE_NAME.encode_utf16().chain(std::iter::once(0)).collect();

    if enabled {
        let Ok(exe_path) = std::env::current_exe() else {
            unsafe { let _ = RegCloseKey(key); }
            return;
        };
        let quoted = format!("\"{}\"", exe_path.display());
        let value: Vec<u16> = quoted.encode_utf16().chain(std::iter::once(0)).collect();
        let bytes: &[u8] = unsafe {
            std::slice::from_raw_parts(value.as_ptr() as *const u8, value.len() * 2)
        };
        // SAFETY: `key` is open with write access; `name`/`bytes` are
        // valid, live buffers for the duration of this call.
        unsafe {
            let _ = RegSetValueExW(key, PCWSTR(name.as_ptr()), 0, REG_SZ, Some(bytes));
        }
    } else {
        // SAFETY: same key/name contract as above.
        unsafe {
            let _ = RegDeleteValueW(key, PCWSTR(name.as_ptr()));
        }
    }

    // SAFETY: `key` was opened by this function and is not used past
    // this point.
    unsafe { let _ = RegCloseKey(key); }
}

#[allow(unused_imports)]
use w as _; // silences an unused-import lint if `w!` ends up unused on some feature combination
