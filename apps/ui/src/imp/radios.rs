//! Bluetooth and Airplane Mode via the WinRT `Windows.Devices.Radios`
//! API — unlike Wi-Fi (`wlanapi.dll`, a plain Win32 call), there's no
//! classic Win32 surface for turning Bluetooth on/off, so this is the
//! one part of Quick Settings that needs a WinRT call. Every call here
//! blocks the calling thread on the WinRT async operation's `.get()`
//! (fine for a UI thread reacting to a single click, not fine for
//! anything hot-path) and degrades to "unavailable" on any failure —
//! missing hardware, access denied, or (for an unpackaged desktop app
//! like this one) the access request itself being refused.

use windows::Devices::Radios::{Radio, RadioAccessStatus, RadioKind, RadioState};

/// `RequestAccessAsync` must succeed before `Radio::GetRadios` returns
/// anything meaningful — an unpackaged Win32 app isn't guaranteed that
/// access, so every entry point checks this first and reports
/// "unavailable" rather than guessing if it's denied.
fn access_allowed() -> bool {
    Radio::RequestAccessAsync()
        .and_then(|op| op.get())
        .map(|status| status == RadioAccessStatus::Allowed)
        .unwrap_or(false)
}

fn all_radios() -> Option<Vec<Radio>> {
    if !access_allowed() {
        return None;
    }
    let radios = Radio::GetRadiosAsync().ok()?.get().ok()?;
    Some(radios.into_iter().collect())
}

fn radio_of_kind(kind: RadioKind) -> Option<Radio> {
    all_radios()?.into_iter().find(|r| r.Kind().map(|k| k == kind).unwrap_or(false))
}

/// `None` when there's no Bluetooth radio to report, access was
/// denied, or the WinRT call otherwise failed.
pub(crate) fn bluetooth_on() -> Option<bool> {
    let radio = radio_of_kind(RadioKind::Bluetooth)?;
    Some(radio.State().ok()? == RadioState::On)
}

pub(crate) fn set_bluetooth_on(on: bool) {
    if let Some(radio) = radio_of_kind(RadioKind::Bluetooth) {
        let state = if on { RadioState::On } else { RadioState::Off };
        let _ = radio.SetStateAsync(state).and_then(|op| op.get());
    }
}

/// Airplane Mode isn't a single radio — there's no public API for the
/// system's own "airplane mode" flag, so this approximates it the same
/// way most third-party toggles do: "on" means every known radio is
/// off. Flipping it back doesn't restore each radio's *previous*
/// individual state (only the real OS-level airplane mode does that);
/// it just turns them all back on together.
pub(crate) fn airplane_mode_on() -> Option<bool> {
    let radios = all_radios()?;
    if radios.is_empty() {
        return None;
    }
    Some(radios.iter().all(|r| r.State().ok() == Some(RadioState::Off)))
}

pub(crate) fn set_airplane_mode_on(on: bool) {
    let Some(radios) = all_radios() else {
        return;
    };
    let state = if on { RadioState::Off } else { RadioState::On };
    for radio in radios {
        let _ = radio.SetStateAsync(state).and_then(|op| op.get());
    }
}
