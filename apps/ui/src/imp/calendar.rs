//! The clock's calendar + notifications flyout.

use windows::Win32::Foundation::{COLORREF, HWND, RECT};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, InvalidateRect, SetBkMode, SetTextColor, PAINTSTRUCT, DT_CENTER,
    DT_SINGLELINE, DT_VCENTER, TRANSPARENT,
};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows::Win32::UI::WindowsAndMessaging::{
    SetForegroundWindow, ShowWindow, SW_HIDE, SW_SHOW,
};

use super::overview::close_overview;
use super::quick_settings::hide_quick_settings;
use super::state::STATE;
use super::util::draw_text_in;

pub(crate) const CAL_WIDTH: i32 = 320;
pub(crate) const CAL_CALENDAR_HEIGHT: i32 = 300;
const CAL_NOTIF_HEIGHT: i32 = 140;
pub(crate) const CAL_HEIGHT: i32 = CAL_CALENDAR_HEIGHT + CAL_NOTIF_HEIGHT;
const CAL_PADDING: i32 = 12;
const CAL_CELL_HEIGHT: i32 = 34;

pub(crate) fn clock_text() -> String {
    // SAFETY: no preconditions.
    let t = unsafe { GetLocalTime() };
    let hour12 = match t.wHour % 12 {
        0 => 12,
        h => h,
    };
    let ampm = if t.wHour < 12 { "AM" } else { "PM" };
    format!("{hour12:02}:{:02} {ampm}", t.wMinute)
}

fn is_leap_year(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

fn days_in_month(year: i32, month: i32) -> i32 {
    const DAYS: [i32; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    if month == 2 && is_leap_year(year) {
        29
    } else {
        DAYS[(month - 1) as usize]
    }
}

fn month_name(month: i32) -> &'static str {
    const NAMES: [&str; 12] = [
        "January", "February", "March", "April", "May", "June", "July", "August",
        "September", "October", "November", "December",
    ];
    NAMES[(month - 1) as usize]
}

/// Draws a real month calendar (today highlighted) over a notifications
/// section. The day-of-week of the 1st is derived from today's own
/// day-of-week/day-of-month rather than a separate date calculation,
/// since the two are always a fixed number of days apart within the
/// same month.
pub(crate) fn paint_calendar(hwnd: HWND) {
    // SAFETY: `hwnd` is the window currently processing `WM_PAINT`.
    unsafe {
        let mut ps = PAINTSTRUCT::default();
        let hdc = BeginPaint(hwnd, &mut ps);
        SetBkMode(hdc, TRANSPARENT);

        let now = GetLocalTime();
        let year = now.wYear as i32;
        let month = now.wMonth as i32;
        let today = now.wDay as i32;
        let today_dow = now.wDayOfWeek as i32;
        let first_dow = ((today_dow - (today - 1)) % 7 + 7) % 7;
        let days = days_in_month(year, month);

        let format = DT_SINGLELINE | DT_VCENTER | DT_CENTER;

        SetTextColor(hdc, COLORREF(0x00FFFFFF));
        draw_text_in(
            hdc,
            RECT {
                left: CAL_PADDING,
                top: 8,
                right: CAL_WIDTH - CAL_PADDING,
                bottom: 32,
            },
            &format!("{} {year}", month_name(month)),
            format,
        );

        let cell_w = (CAL_WIDTH - CAL_PADDING * 2) / 7;
        const DOW_LABELS: [&str; 7] = ["Su", "Mo", "Tu", "We", "Th", "Fr", "Sa"];
        SetTextColor(hdc, COLORREF(0x00A0A0A0));
        for (i, label) in DOW_LABELS.iter().enumerate() {
            let x = CAL_PADDING + i as i32 * cell_w;
            draw_text_in(
                hdc,
                RECT {
                    left: x,
                    top: 40,
                    right: x + cell_w,
                    bottom: 60,
                },
                label,
                format,
            );
        }

        let mut day = 1;
        let mut col = first_dow;
        let mut row = 0;
        while day <= days {
            let x = CAL_PADDING + col * cell_w;
            let y = 64 + row * CAL_CELL_HEIGHT;
            SetTextColor(
                hdc,
                if day == today {
                    COLORREF(0x0040A0FF)
                } else {
                    COLORREF(0x00E0E0E0)
                },
            );
            draw_text_in(
                hdc,
                RECT {
                    left: x,
                    top: y,
                    right: x + cell_w,
                    bottom: y + CAL_CELL_HEIGHT,
                },
                &day.to_string(),
                format,
            );
            day += 1;
            col += 1;
            if col == 7 {
                col = 0;
                row += 1;
            }
        }

        let notif_format = DT_SINGLELINE | DT_VCENTER;
        SetTextColor(hdc, COLORREF(0x00FFFFFF));
        draw_text_in(
            hdc,
            RECT {
                left: CAL_PADDING,
                top: CAL_CALENDAR_HEIGHT + 10,
                right: CAL_WIDTH - CAL_PADDING,
                bottom: CAL_CALENDAR_HEIGHT + 34,
            },
            "Notifications",
            notif_format,
        );
        SetTextColor(hdc, COLORREF(0x00A0A0A0));
        draw_text_in(
            hdc,
            RECT {
                left: CAL_PADDING,
                top: CAL_CALENDAR_HEIGHT + 40,
                right: CAL_WIDTH - CAL_PADDING,
                bottom: CAL_CALENDAR_HEIGHT + 64,
            },
            "No new notifications",
            notif_format,
        );

        let _ = EndPaint(hwnd, &ps);
    }
}

/// Hides the calendar flyout if it's open. `restore_focus` should be
/// `true` for an explicit dismiss (toggle-off click, Escape) and
/// `false` when it's being closed because another flyout is about to
/// take over (that flyout will own focus next) or because it's losing
/// activation naturally (the user already clicked something else,
/// which is already becoming foreground on its own — forcing our
/// stashed `previous_foreground` back at that moment would fight the
/// click that just happened).
pub(crate) fn hide_calendar(restore_focus: bool) {
    let result = STATE.with(|s| {
        let mut state_ref = s.borrow_mut();
        let state = state_ref.as_mut()?;
        if !state.calendar_open {
            return None;
        }
        state.calendar_open = false;
        Some((state.calendar_hwnd, state.previous_foreground))
    });
    let Some((hwnd, previous)) = result else {
        return;
    };
    // SAFETY: `hwnd` is a valid, process-lifetime window; `previous`
    // (if used) was captured moments-to-minutes ago by
    // `GetForegroundWindow` and may have since closed, in which case
    // `SetForegroundWindow` documented-fails rather than misbehaving.
    unsafe {
        let _ = ShowWindow(hwnd, SW_HIDE);
        if restore_focus && !previous.0.is_null() {
            let _ = SetForegroundWindow(previous);
        }
    }
}

pub(crate) fn toggle_calendar() {
    let info = STATE.with(|s| {
        s.borrow()
            .as_ref()
            .map(|st| (st.calendar_hwnd, st.calendar_open, st.primary_monitor.clone()))
    });
    let Some((hwnd, is_open, primary_monitor)) = info else {
        return;
    };

    if is_open {
        hide_calendar(true);
        return;
    }

    hide_quick_settings(false);
    close_overview(&primary_monitor, None);

    // SAFETY: no preconditions.
    let previous_foreground = unsafe { windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow() };
    STATE.with(|s| {
        if let Some(state) = s.borrow_mut().as_mut() {
            state.previous_foreground = previous_foreground;
            state.calendar_open = true;
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

#[cfg(test)]
mod tests {
    use super::{days_in_month, is_leap_year, month_name};

    #[test]
    fn leap_years_follow_the_gregorian_rule() {
        assert!(is_leap_year(2024)); // divisible by 4
        assert!(!is_leap_year(1900)); // divisible by 100, not 400
        assert!(is_leap_year(2000)); // divisible by 400
        assert!(!is_leap_year(2023)); // not divisible by 4
    }

    #[test]
    fn february_has_29_days_in_a_leap_year_and_28_otherwise() {
        assert_eq!(days_in_month(2024, 2), 29);
        assert_eq!(days_in_month(2023, 2), 28);
    }

    #[test]
    fn days_in_month_matches_the_calendar_for_every_month() {
        let expected = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
        for (i, &days) in expected.iter().enumerate() {
            assert_eq!(days_in_month(2023, i as i32 + 1), days);
        }
    }

    #[test]
    fn month_name_returns_the_full_english_name() {
        assert_eq!(month_name(1), "January");
        assert_eq!(month_name(12), "December");
    }
}
