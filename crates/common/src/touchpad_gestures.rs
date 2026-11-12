//! Borrowing the three- and four-finger swipes from Windows, and giving
//! them back.
//!
//! The shell reads those swipes itself, but Raw Input cannot stop
//! Windows acting on them too: without this, a swipe up would open
//! Activities and Task View together. The only switch Windows offers is
//! the pair of per-user settings behind Settings > Touchpad >
//! Three-/Four-finger gestures, so the shell sets both to "Nothing"
//! while it runs.
//!
//! These are the user's settings, changed on their behalf, so the same
//! care applies as for the taskbar's auto-hide state (`taskbar_state`):
//! what was there before is written to a file *before* anything is
//! changed, a saved file always wins over the live values, and both the
//! shell and `groveshell-cli restore-taskbar` can put it back.

use std::path::{Path, PathBuf};

/// The settings taken over. Each is a `DWORD` under
/// `HKCU\Software\Microsoft\Windows\CurrentVersion\PrecisionTouchPad`
/// where 0 means "Nothing"; a missing value means the Windows default.
pub const VALUES: [&str; 2] = ["ThreeFingerSlideEnabled", "FourFingerSlideEnabled"];

/// One setting as it was found: `None` if the value did not exist.
pub type Setting = (String, Option<u32>);

/// `<data_dir>/touchpad-gestures`.
pub fn path() -> crate::Result<PathBuf> {
    Ok(crate::paths::data_dir()?.join("touchpad-gestures"))
}

/// One `name=value` line per setting, `name=absent` for a value that
/// did not exist. Absent is recorded, not skipped: restoring has to
/// delete the value again, not leave the shell's zero behind.
pub fn format(settings: &[Setting]) -> String {
    settings
        .iter()
        .map(|(name, value)| match value {
            Some(v) => format!("{name}={v}\n"),
            None => format!("{name}=absent\n"),
        })
        .collect()
}

/// `None` for anything that is not a complete record of [`VALUES`]: a
/// half-written file must not restore half the settings and then be
/// cleared as if the job were done.
pub fn parse(text: &str) -> Option<Vec<Setting>> {
    let mut settings = Vec::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let (name, value) = line.trim().split_once('=')?;
        let value = if value == "absent" { None } else { Some(value.parse::<u32>().ok()?) };
        settings.push((name.to_string(), value));
    }
    VALUES
        .iter()
        .all(|wanted| settings.iter().any(|(name, _)| name == wanted))
        .then_some(settings)
}

pub fn read(path: &Path) -> Option<Vec<Setting>> {
    parse(&std::fs::read_to_string(path).ok()?)
}

/// Returns whether the record reached the disk. The caller must not
/// change anything if it did not.
pub fn write(path: &Path, settings: &[Setting]) -> bool {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match std::fs::write(path, format(settings)) {
        Ok(()) => true,
        Err(e) => {
            tracing::warn!(error = ?e, "could not record the touchpad gesture settings; leaving them alone");
            false
        }
    }
}

pub fn clear(path: &Path) {
    let _ = std::fs::remove_file(path);
}

#[cfg(windows)]
mod imp {
    use super::*;
    use windows::core::{HSTRING, PCWSTR};
    use windows::Win32::Foundation::{ERROR_SUCCESS, LPARAM, WPARAM};
    use windows::Win32::System::Registry::{
        RegCloseKey, RegCreateKeyExW, RegDeleteValueW, RegQueryValueExW, RegSetValueExW, HKEY,
        HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_DWORD, REG_OPTION_NON_VOLATILE,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        SendMessageTimeoutW, SystemParametersInfoW, HWND_BROADCAST, SMTO_ABORTIFHUNG,
        SPIF_SENDCHANGE, SYSTEM_PARAMETERS_INFO_ACTION, WM_SETTINGCHANGE,
    };

    const KEY: PCWSTR = windows::core::w!(r"Software\Microsoft\Windows\CurrentVersion\PrecisionTouchPad");

    fn open() -> Option<HKEY> {
        let mut key = HKEY::default();
        // SAFETY: `key` is a live out-parameter; the key is created if
        // it is missing, which only happens on a machine with no
        // precision touchpad, where the values are inert anyway.
        let status = unsafe {
            RegCreateKeyExW(
                HKEY_CURRENT_USER,
                KEY,
                0,
                None,
                REG_OPTION_NON_VOLATILE,
                KEY_READ | KEY_WRITE,
                None,
                &mut key,
                None,
            )
        };
        (status == ERROR_SUCCESS).then_some(key)
    }

    fn query(key: HKEY, name: &str) -> Option<u32> {
        let name = HSTRING::from(name);
        let mut data = [0u8; 4];
        let mut len = data.len() as u32;
        // SAFETY: `data`/`len` describe a live four-byte buffer.
        let status = unsafe {
            RegQueryValueExW(key, &name, None, None, Some(data.as_mut_ptr()), Some(&mut len))
        };
        (status == ERROR_SUCCESS && len == 4).then(|| u32::from_ne_bytes(data))
    }

    /// The settings as they are right now.
    pub fn current() -> Option<Vec<Setting>> {
        let key = open()?;
        let settings = VALUES.iter().map(|name| (name.to_string(), query(key, name))).collect();
        // SAFETY: opened above.
        unsafe {
            let _ = RegCloseKey(key);
        }
        Some(settings)
    }

    fn apply(settings: &[Setting]) -> bool {
        let Some(key) = open() else { return false };
        let mut ok = true;
        for (name, value) in settings {
            let wide = HSTRING::from(name.as_str());
            // SAFETY: `key` is open with write access; the data is a
            // live four-byte local.
            let status = unsafe {
                match value {
                    Some(v) => RegSetValueExW(key, &wide, 0, REG_DWORD, Some(&v.to_ne_bytes())),
                    None => RegDeleteValueW(key, &wide),
                }
            };
            // Deleting a value that is already gone is the state asked for.
            let missing = windows::Win32::Foundation::ERROR_FILE_NOT_FOUND;
            ok &= status == ERROR_SUCCESS || (value.is_none() && status == missing);
        }
        // SAFETY: opened above.
        unsafe {
            let _ = RegCloseKey(key);
        }
        nudge();
        ok
    }

    /// Asks Windows to notice the change.
    ///
    /// Nothing documents how these two values are picked up. Settings
    /// applies them at once; a bare registry write is reported to work
    /// on some builds and to wait for the next sign-in on others. Two
    /// harmless things are tried, neither of them known to be what
    /// Settings does: writing the documented touchpad parameters back
    /// unchanged, in case that makes the input stack reload the rest of
    /// its touchpad settings (the call exists from 24H2 on), and the
    /// ordinary settings-changed broadcast.
    fn nudge() {
        const SPI_GETTOUCHPADPARAMETERS: SYSTEM_PARAMETERS_INFO_ACTION = SYSTEM_PARAMETERS_INFO_ACTION(0x00AE);
        const SPI_SETTOUCHPADPARAMETERS: SYSTEM_PARAMETERS_INFO_ACTION = SYSTEM_PARAMETERS_INFO_ACTION(0x00AF);
        // `TOUCHPAD_PARAMETERS`, version 1: eleven 32-bit fields, the
        // first of which is the version. Treated as opaque — it is read
        // and written back untouched.
        let mut params = [0u32; 11];
        params[0] = 1;
        let size = std::mem::size_of_val(&params) as u32;
        // SAFETY: `params` is a live buffer of the size passed. An older
        // build rejects the unknown action and changes nothing.
        unsafe {
            let got = SystemParametersInfoW(
                SPI_GETTOUCHPADPARAMETERS,
                size,
                Some(params.as_mut_ptr() as *mut core::ffi::c_void),
                Default::default(),
            )
            .is_ok();
            let reapplied = got
                && SystemParametersInfoW(
                    SPI_SETTOUCHPADPARAMETERS,
                    size,
                    Some(params.as_mut_ptr() as *mut core::ffi::c_void),
                    SPIF_SENDCHANGE,
                )
                .is_ok();
            tracing::info!(got, reapplied, "touchpad gestures: asked Windows to reload touchpad settings");

            let _ = SendMessageTimeoutW(
                HWND_BROADCAST,
                WM_SETTINGCHANGE,
                WPARAM(0),
                LPARAM(windows::core::w!("PrecisionTouchPad").as_ptr() as isize),
                SMTO_ABORTIFHUNG,
                200,
                None,
            );
        }
    }

    /// Turns Windows' own three- and four-finger swipes off, recording
    /// what they were first. Returns whether they are now off.
    pub fn suppress() -> bool {
        let Ok(path) = path() else { return false };
        let Some(live) = current() else { return false };
        // A record already on disk is from a run that never gave the
        // gestures back: the live zeros are ours, not the user's.
        if read(&path).is_none() && !write(&path, &live) {
            return false;
        }
        let off: Vec<Setting> = VALUES.iter().map(|n| (n.to_string(), Some(0))).collect();
        let ok = apply(&off);
        tracing::info!(ok, before = ?live, "touchpad gestures: Windows' swipes switched off");
        ok
    }

    /// Puts the settings back as recorded. `None` if there was nothing
    /// recorded to put back.
    pub fn restore() -> Option<Vec<Setting>> {
        let path = path().ok()?;
        let saved = read(&path)?;
        if apply(&saved) {
            clear(&path);
        }
        tracing::info!(restored = ?saved, "touchpad gestures: Windows' swipes given back");
        Some(saved)
    }
}

#[cfg(windows)]
pub use imp::{current, restore, suppress};

#[cfg(test)]
mod tests {
    use super::*;

    fn both(three: Option<u32>, four: Option<u32>) -> Vec<Setting> {
        vec![(VALUES[0].to_string(), three), (VALUES[1].to_string(), four)]
    }

    #[test]
    fn the_record_round_trips_including_absent_values() {
        for settings in [both(Some(1), Some(2)), both(None, None), both(Some(0), None)] {
            assert_eq!(parse(&format(&settings)), Some(settings));
        }
    }

    /// The defaults are *absent* values. Recording them as nothing at
    /// all would leave the shell's zeros in place after a restore, and
    /// the user's gestures off for good.
    #[test]
    fn an_absent_value_is_recorded_not_skipped() {
        assert_eq!(format(&both(None, Some(2))), "ThreeFingerSlideEnabled=absent\nFourFingerSlideEnabled=2\n");
    }

    #[test]
    fn a_partial_or_corrupt_record_is_not_trusted() {
        assert_eq!(parse(""), None);
        assert_eq!(parse("ThreeFingerSlideEnabled=1\n"), None, "one of two");
        assert_eq!(parse("ThreeFingerSlideEnabled=1\nFourFingerSlideEnabled=two\n"), None);
        assert_eq!(parse("garbage"), None);
    }

    #[test]
    fn the_record_survives_its_file() {
        let dir = std::env::temp_dir().join(format!("gs-tpgest-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let p = dir.join("touchpad-gestures");
        assert_eq!(read(&p), None);
        assert!(write(&p, &both(None, Some(2))));
        assert_eq!(read(&p), Some(both(None, Some(2))));
        clear(&p);
        assert_eq!(read(&p), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
