//! The Quick Settings flyout: real volume control and read-only battery
//! status.

use windows::Win32::Foundation::{COLORREF, HWND, RECT};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, InvalidateRect, SetBkMode, SetTextColor, PAINTSTRUCT, DT_SINGLELINE,
    DT_VCENTER, TRANSPARENT,
};
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{eConsole, eRender, IMMDeviceEnumerator, MMDeviceEnumerator};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_ALL};
use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, SetForegroundWindow, ShowWindow, SW_HIDE, SW_SHOW,
};

use super::calendar::hide_calendar;
use super::overview::close_overview;
use super::state::STATE;
use super::util::draw_text_in;

pub(crate) const QS_WIDTH: i32 = 280;
pub(crate) const QS_HEIGHT: i32 = 170;
pub(crate) const QS_PADDING: i32 = 16;
pub(crate) const QS_VOL_DOWN: i32 = 2001;
pub(crate) const QS_VOL_UP: i32 = 2002;
pub(crate) const QS_MUTE: i32 = 2003;

pub(crate) fn quick_settings_label_text() -> String {
    match battery_percent() {
        Some(pct) => format!("{pct}%  Quick Settings"),
        None => "Quick Settings".to_string(),
    }
}

/// `None` when there's no battery to report (desktop on AC), not on
/// I/O failure — `GetSystemPowerStatus` reports `255` for "unknown",
/// which covers both cases; either way there's nothing meaningful to
/// show.
pub(crate) fn battery_percent() -> Option<u8> {
    // SAFETY: `status` is a local, zeroed `SYSTEM_POWER_STATUS` that
    // outlives this synchronous call.
    unsafe {
        let mut status = SYSTEM_POWER_STATUS::default();
        GetSystemPowerStatus(&mut status).ok()?;
        (status.BatteryLifePercent != 255).then_some(status.BatteryLifePercent)
    }
}

pub(crate) fn paint_quick_settings(hwnd: HWND) {
    // SAFETY: `hwnd` is the window currently processing `WM_PAINT`.
    unsafe {
        let mut ps = PAINTSTRUCT::default();
        let hdc = BeginPaint(hwnd, &mut ps);
        SetBkMode(hdc, TRANSPARENT);

        let format = DT_SINGLELINE | DT_VCENTER;

        SetTextColor(hdc, COLORREF(0x00FFFFFF));
        draw_text_in(
            hdc,
            RECT {
                left: QS_PADDING,
                top: 10,
                right: QS_WIDTH - QS_PADDING,
                bottom: 34,
            },
            "Quick Settings",
            format,
        );

        let volume_text = match (get_volume_percent(), get_mute()) {
            (Some(pct), Some(true)) => format!("Volume: {pct}% (Muted)"),
            (Some(pct), _) => format!("Volume: {pct}%"),
            (None, _) => "Volume: unavailable".to_string(),
        };
        draw_text_in(
            hdc,
            RECT {
                left: QS_PADDING,
                top: 46,
                right: QS_WIDTH - QS_PADDING,
                bottom: 70,
            },
            &volume_text,
            format,
        );

        SetTextColor(hdc, COLORREF(0x00A0A0A0));
        let battery_text = match battery_percent() {
            Some(pct) => format!("Battery: {pct}%"),
            None => "On AC power".to_string(),
        };
        draw_text_in(
            hdc,
            RECT {
                left: QS_PADDING,
                top: 130,
                right: QS_WIDTH - QS_PADDING,
                bottom: 154,
            },
            &battery_text,
            format,
        );

        let _ = EndPaint(hwnd, &ps);
    }
}

/// Acquires the default audio endpoint's volume control fresh for each
/// call rather than caching it — simpler and more robust against the
/// default device changing than holding a long-lived COM object, at
/// the cost of a little overhead per volume interaction (negligible;
/// this only ever runs in response to a button click).
fn with_volume<R>(f: impl FnOnce(&IAudioEndpointVolume) -> windows::core::Result<R>) -> Option<R> {
    // SAFETY: `CoInitializeEx` was called once at process startup on
    // this same thread; every call here is synchronous and its result
    // fully consumed before returning.
    unsafe {
        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok()?;
        let device = enumerator.GetDefaultAudioEndpoint(eRender, eConsole).ok()?;
        let volume: IAudioEndpointVolume = device.Activate(CLSCTX_ALL, None).ok()?;
        f(&volume).ok()
    }
}

pub(crate) fn get_volume_percent() -> Option<u32> {
    with_volume(|v| unsafe { v.GetMasterVolumeLevelScalar() })
        .map(|scalar| (scalar * 100.0).round() as u32)
}

pub(crate) fn get_mute() -> Option<bool> {
    with_volume(|v| unsafe { v.GetMute() }).map(|b| b.as_bool())
}

pub(crate) fn adjust_volume(delta_percent: i32) {
    let Some(current) = get_volume_percent() else {
        return;
    };
    let next = (current as i32 + delta_percent).clamp(0, 100) as f32 / 100.0;
    // SAFETY: no preconditions beyond `with_volume`'s own.
    let _ = with_volume(|v| unsafe { v.SetMasterVolumeLevelScalar(next, std::ptr::null()) });
}

pub(crate) fn toggle_mute() {
    let Some(muted) = get_mute() else {
        return;
    };
    // SAFETY: no preconditions beyond `with_volume`'s own.
    let _ = with_volume(|v| unsafe { v.SetMute(!muted, std::ptr::null()) });
}

/// Mirrors [`hide_calendar`] for the Quick Settings flyout.
pub(crate) fn hide_quick_settings(restore_focus: bool) {
    let result = STATE.with(|s| {
        let mut state_ref = s.borrow_mut();
        let state = state_ref.as_mut()?;
        if !state.quick_settings_open {
            return None;
        }
        state.quick_settings_open = false;
        Some((state.quick_settings_hwnd, state.previous_foreground))
    });
    let Some((hwnd, previous)) = result else {
        return;
    };
    // SAFETY: see `hide_calendar`.
    unsafe {
        let _ = ShowWindow(hwnd, SW_HIDE);
        if restore_focus && !previous.0.is_null() {
            let _ = SetForegroundWindow(previous);
        }
    }
}

pub(crate) fn toggle_quick_settings() {
    let info = STATE.with(|s| {
        s.borrow()
            .as_ref()
            .map(|st| (st.quick_settings_hwnd, st.quick_settings_open))
    });
    let Some((hwnd, is_open)) = info else {
        return;
    };

    if is_open {
        hide_quick_settings(true);
        return;
    }

    hide_calendar(false);
    close_overview(None);

    // SAFETY: no preconditions.
    let previous_foreground = unsafe { GetForegroundWindow() };
    STATE.with(|s| {
        if let Some(state) = s.borrow_mut().as_mut() {
            state.previous_foreground = previous_foreground;
            state.quick_settings_open = true;
        }
    });

    // SAFETY: `hwnd` is a valid, process-lifetime window.
    unsafe {
        let _ = InvalidateRect(hwnd, None, true);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(hwnd);
    }
}
