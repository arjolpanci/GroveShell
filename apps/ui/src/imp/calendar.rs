//! The clock's calendar + notifications flyout.

use windows::Win32::Foundation::{COLORREF, HWND, RECT};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, EndPaint, InvalidateRect, PAINTSTRUCT, DT_CENTER, DT_LEFT, DT_SINGLELINE,
    DT_VCENTER,
};
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows::Win32::UI::WindowsAndMessaging::{
    SetForegroundWindow, SetTimer, ShowWindow, SW_SHOW,
};

use super::overview::close_overview;
use super::quick_settings::hide_quick_settings;
use super::canvas::Canvas;
use super::glyph;
use super::state::{scaled, STATE};

pub(crate) const CAL_WIDTH: i32 = 320;
pub(crate) const CAL_CALENDAR_HEIGHT: i32 = 300;
const CAL_NOTIF_HEIGHT: i32 = 140;
pub(crate) const CAL_HEIGHT: i32 = CAL_CALENDAR_HEIGHT + CAL_NOTIF_HEIGHT;
const CAL_PADDING: i32 = 12;
const CAL_CELL_HEIGHT: i32 = 34;
/// Height of the month/year title band at the top of the flyout.
const CAL_HEADER_TOP: i32 = 8;
const CAL_HEADER_HEIGHT: i32 = 28;
/// The previous/next month chevrons, as a square.
const CAL_CHEVRON_SIZE: i32 = 20;
/// The row of weekday initials under the title.
const CAL_WEEKDAY_HEIGHT: i32 = 20;
/// The "Notifications" caption above the notification area.
const CAL_NOTIF_HEADER_HEIGHT: i32 = 28;
/// Opacity of the card on the Direct2D backend — the same value Quick
/// Settings uses, so the two flyouts read as one material.
const CAL_CARD_ALPHA: f32 = 0.78;

/// Every rectangle the calendar draws, at one DPI.
///
/// Pure arithmetic: painting and hit-testing both read this, so the
/// month grid they describe can never disagree — the same rule
/// `qs_layout` and the settings rows follow.
pub(crate) struct CalLayout {
    pub(crate) card: RECT,
    /// The month-and-year title, between the two chevrons.
    pub(crate) header: RECT,
    pub(crate) prev: RECT,
    pub(crate) next: RECT,
    pub(crate) weekdays: [RECT; 7],
    /// Top of the first row of day cells.
    pub(crate) grid_top: i32,
    pub(crate) cell_width: i32,
    pub(crate) cell_height: i32,
    pub(crate) notif_header: RECT,
    pub(crate) notif_body: RECT,
}

/// Lays the flyout out at `dpi`.
///
/// Everything below is a *logical* measurement scaled at the point of
/// use. Before this, the whole file used raw constants and the flyout
/// drew at half size on a 200% display.
pub(crate) fn cal_layout(dpi: u32) -> CalLayout {
    let pad = scaled(CAL_PADDING, dpi);
    let width = scaled(CAL_WIDTH, dpi);
    let card = RECT { left: 0, top: 0, right: width, bottom: scaled(CAL_HEIGHT, dpi) };

    let chevron = scaled(CAL_CHEVRON_SIZE, dpi);
    let header_top = scaled(CAL_HEADER_TOP, dpi);
    let header_height = scaled(CAL_HEADER_HEIGHT, dpi);
    let header_mid = header_top + header_height / 2;
    let prev = RECT {
        left: pad,
        top: header_mid - chevron / 2,
        right: pad + chevron,
        bottom: header_mid + chevron / 2,
    };
    let next = RECT {
        left: width - pad - chevron,
        top: prev.top,
        right: width - pad,
        bottom: prev.bottom,
    };
    let header = RECT {
        left: prev.right + scaled(4, dpi),
        top: header_top,
        right: next.left - scaled(4, dpi),
        bottom: header_top + header_height,
    };

    let cell_width = (width - pad * 2) / 7;
    let cell_height = scaled(CAL_CELL_HEIGHT, dpi);
    let weekday_top = header.bottom + scaled(4, dpi);
    let weekday_height = scaled(CAL_WEEKDAY_HEIGHT, dpi);
    let mut weekdays = [RECT::default(); 7];
    for (i, slot) in weekdays.iter_mut().enumerate() {
        let left = pad + i as i32 * cell_width;
        *slot = RECT {
            left,
            top: weekday_top,
            right: left + cell_width,
            bottom: weekday_top + weekday_height,
        };
    }

    let grid_top = weekday_top + weekday_height + scaled(2, dpi);
    let notif_top = scaled(CAL_CALENDAR_HEIGHT, dpi);
    let notif_header = RECT {
        left: pad,
        top: notif_top,
        right: width - pad,
        bottom: notif_top + scaled(CAL_NOTIF_HEADER_HEIGHT, dpi),
    };
    let notif_body = RECT {
        left: pad,
        top: notif_header.bottom,
        right: width - pad,
        bottom: card.bottom - pad,
    };

    CalLayout {
        card,
        header,
        prev,
        next,
        weekdays,
        grid_top,
        cell_width,
        cell_height,
        notif_header,
        notif_body,
    }
}

/// The cell for `day` (1-based) in a month whose 1st falls on weekday
/// `first_dow` (0 = Sunday).
pub(crate) fn day_cell(layout: &CalLayout, first_dow: i32, day: i32) -> RECT {
    let index = first_dow + day - 1;
    let row = index / 7;
    let col = index % 7;
    let left = layout.weekdays[0].left + col * layout.cell_width;
    let top = layout.grid_top + row * layout.cell_height;
    RECT {
        left,
        top,
        right: left + layout.cell_width,
        bottom: top + layout.cell_height,
    }
}

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

thread_local! {
    /// How many months away from the current one the grid is showing.
    /// Reset every time the flyout opens, so it always comes up on
    /// today rather than wherever it was left.
    static MONTH_OFFSET: std::cell::Cell<i32> = const { std::cell::Cell::new(0) };
}

/// `(year, month)` shifted by `offset` months, rolling the year over in
/// either direction. Month is 1-based.
fn shift_month(year: i32, month: i32, offset: i32) -> (i32, i32) {
    let zero_based = (month - 1) + offset;
    // Rust's `%` keeps the sign of the dividend, so a negative offset
    // needs the extra wrap to land back in 0..12.
    let wrapped = zero_based.rem_euclid(12);
    let years = (zero_based - wrapped) / 12;
    (year + years, wrapped + 1)
}

/// Steps the grid `delta` months and repaints.
pub(crate) fn step_month(hwnd: HWND, delta: i32) {
    MONTH_OFFSET.with(|m| m.set(m.get() + delta));
    // SAFETY: `hwnd` is a live, process-lifetime window.
    unsafe {
        let _ = InvalidateRect(hwnd, None, false);
    }
}

/// A click inside the flyout. Only the month chevrons are interactive;
/// anything else is ignored rather than dismissing, so a stray click on
/// the grid does not close the panel.
pub(crate) fn on_calendar_click(hwnd: HWND, x: i32, y: i32) {
    // SAFETY: plain query on a live window.
    let dpi = unsafe { GetDpiForWindow(hwnd).max(96) };
    let l = cal_layout(dpi);
    let hit = |r: RECT| x >= r.left && x < r.right && y >= r.top && y < r.bottom;
    if hit(l.prev) {
        step_month(hwnd, -1);
    } else if hit(l.next) {
        step_month(hwnd, 1);
    }
}

/// The weekday the 1st of `(year, month)` falls on, walked from a month
/// whose answer is already known.
///
/// `GetLocalTime` hands back today's weekday, which gives the current
/// month's 1st for free; every other month is that answer plus or minus
/// the days in between. Walking avoids a calendar algorithm — and the
/// off-by-one bugs that come with one — at the cost of a loop that is
/// never long in practice, since the grid moves a month at a time.
fn first_dow_of(
    base_year: i32,
    base_month: i32,
    base_first_dow: i32,
    year: i32,
    month: i32,
) -> i32 {
    let mut dow = base_first_dow;
    let (mut y, mut m) = (base_year, base_month);
    while (y, m) < (year, month) {
        dow = (dow + days_in_month(y, m)) % 7;
        let (ny, nm) = shift_month(y, m, 1);
        y = ny;
        m = nm;
    }
    while (y, m) > (year, month) {
        let (py, pm) = shift_month(y, m, -1);
        dow = (dow - days_in_month(py, pm)).rem_euclid(7);
        y = py;
        m = pm;
    }
    dow
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

/// Draws the calendar's content (background, header, day grid,
/// notifications section) through the Direct2D wrapper — the GPU-path
/// equivalent of the GDI drawing `paint_calendar` does below. Every
/// coordinate and color here is copied from `paint_calendar` unchanged;
/// this must stay a faithful port, not a redesign.
/// Draws the whole flyout through [`Canvas`], so the Direct2D and GDI
/// backends render from one source.
///
/// Before this, the calendar had two painters that had to be kept
/// identical by hand — the duplication `Canvas` exists to remove (see
/// ADR-010). Every measurement is logical and scaled at the point of
/// use; the flyout used to draw at raw constants, which meant half size
/// on a 200% display.
fn render_calendar(c: &mut dyn Canvas, dpi: u32) {
    let l = cal_layout(dpi);

    // SAFETY: no preconditions.
    let now = unsafe { GetLocalTime() };
    let offset = MONTH_OFFSET.with(|m| m.get());
    let (year, month) = shift_month(now.wYear as i32, now.wMonth as i32, offset);
    let days = days_in_month(year, month);

    // The weekday of the 1st: known directly for the current month, and
    // walked from it for any other, so no calendar algorithm is needed.
    let this_first = ((now.wDayOfWeek as i32 - (now.wDay as i32 - 1)) % 7 + 7) % 7;
    let first_dow = first_dow_of(now.wYear as i32, now.wMonth as i32, this_first, year, month);

    // Today is only a thing when the grid is showing today's month.
    let today = if offset == 0 { now.wDay as i32 } else { 0 };

    // The card itself. On the Direct2D backend the surface was cleared to
    // alpha 0, so this translucent fill is what lets the Acrylic behind
    // the window show through; on GDI it is simply opaque.
    c.fill_round_rect_alpha(
        l.card,
        scaled(super::design::metrics::RADIUS_CARD, dpi),
        COLORREF(super::design::color::surface_base()),
        CAL_CARD_ALPHA,
    );

    // Month and year, with a chevron either side.
    c.set_text_color(COLORREF(super::design::color::text()));
    c.set_font_size(super::design::typography::BODY_PX);
    c.text(
        l.header,
        &format!("{} {year}", month_name(month)),
        DT_CENTER | DT_SINGLELINE | DT_VCENTER,
    );
    c.set_text_color(COLORREF(super::design::color::text_muted()));
    c.glyph(l.prev, glyph::CHEVRON_LEFT);
    c.glyph(l.next, glyph::CHEVRON_RIGHT);

    // Weekday initials.
    const DOW_LABELS: [&str; 7] = ["Su", "Mo", "Tu", "We", "Th", "Fr", "Sa"];
    c.set_font_size(super::design::typography::CAPTION_PX);
    for (slot, label) in l.weekdays.iter().zip(DOW_LABELS) {
        c.text(*slot, label, DT_CENTER | DT_SINGLELINE | DT_VCENTER);
    }

    // The day grid. Today gets a filled accent pill with contrasting
    // text, the way Windows marks it — not merely a colored numeral,
    // which reads as "this day is a link".
    c.set_font_size(super::design::typography::BODY_PX);
    for day in 1..=days {
        let cell = day_cell(&l, first_dow, day);
        let color = if day == today {
            let size = (cell.bottom - cell.top).min(cell.right - cell.left);
            let cx = (cell.left + cell.right) / 2;
            let cy = (cell.top + cell.bottom) / 2;
            c.fill_round_rect(
                RECT {
                    left: cx - size / 2,
                    top: cy - size / 2,
                    right: cx + size / 2,
                    bottom: cy + size / 2,
                },
                size / 2,
                COLORREF(super::design::color::accent()),
            );
            super::design::color::accent_text()
        } else {
            super::design::color::text()
        };
        c.set_text_color(COLORREF(color));
        c.text(cell, &day.to_string(), DT_CENTER | DT_SINGLELINE | DT_VCENTER);
    }

    // The notifications section: a caption, a hairline, and an honest
    // empty state. Reading the real feed needs `UserNotificationListener`
    // and a packaged identity this process does not have.
    c.fill_rect(
        RECT {
            left: l.notif_header.left,
            top: l.notif_header.top,
            right: l.notif_header.right,
            bottom: l.notif_header.top + scaled(1, dpi).max(1),
        },
        COLORREF(super::design::color::stroke()),
    );
    c.set_text_color(COLORREF(super::design::color::text()));
    c.set_font_size(super::design::typography::CAPTION_PX);
    c.text(l.notif_header, "Notifications", DT_LEFT | DT_SINGLELINE | DT_VCENTER);
    c.set_text_color(COLORREF(super::design::color::text_muted()));
    c.set_font_size(super::design::typography::BODY_PX);
    c.text(
        RECT { bottom: l.notif_body.top + scaled(28, dpi), ..l.notif_body },
        "No new notifications",
        DT_LEFT | DT_SINGLELINE | DT_VCENTER,
    );
}

/// Paints the flyout through the Direct2D backend, which is the
/// translucent path.
pub(crate) fn paint_calendar_gpu(ctx: &windows::Win32::Graphics::Direct2D::ID2D1DeviceContext, dpi: u32) {
    super::gpu::clear_transparent(ctx);
    render_calendar(&mut super::canvas::D2DCanvas::new(ctx, dpi), dpi);
}

/// Paints the flyout through GDI, into a back buffer so no half-drawn
/// frame reaches the screen. Only reached when the window has no
/// composition surface.
pub(crate) fn paint_calendar(hwnd: HWND) {
    use windows::Win32::Graphics::Gdi::{
        BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, SelectObject,
        SRCCOPY,
    };
    // SAFETY: `hwnd` is the window currently processing `WM_PAINT`;
    // every GDI object created here is selected out and deleted before
    // returning.
    unsafe {
        let mut ps = PAINTSTRUCT::default();
        let target = BeginPaint(hwnd, &mut ps);
        let dpi = GetDpiForWindow(hwnd).max(96);
        let mut client = RECT::default();
        let _ = windows::Win32::UI::WindowsAndMessaging::GetClientRect(hwnd, &mut client);

        let buffer = CreateCompatibleDC(target);
        let bitmap = CreateCompatibleBitmap(target, client.right, client.bottom);
        if buffer.0.is_null() || bitmap.0.is_null() {
            render_calendar(&mut super::canvas::GdiCanvas::new(target, dpi), dpi);
        } else {
            let old = SelectObject(buffer, bitmap);
            render_calendar(&mut super::canvas::GdiCanvas::new(buffer, dpi), dpi);
            let _ = BitBlt(target, 0, 0, client.right, client.bottom, buffer, 0, 0, SRCCOPY);
            SelectObject(buffer, old);
        }
        let _ = DeleteObject(bitmap);
        let _ = DeleteDC(buffer);
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
/// Timer driving the dropdown reveal, at the same 16ms cadence Quick
/// Settings uses.
pub(crate) const CAL_TIMER_ID: usize = 72;

thread_local! {
    /// The calendar's open/close lifecycle. Shares `flyout::Flyout` with
    /// Quick Settings so both flyouts unroll with one motion system
    /// (spec §5).
    static MOTION: std::cell::RefCell<super::flyout::Flyout> =
        std::cell::RefCell::new(super::flyout::Flyout::new());
}

/// Resizes the calendar to its current unroll height.
///
/// Like Quick Settings, the window's top stays pinned under the bar and
/// only its height moves. The Direct2D surface is deliberately *not*
/// resized — it is created once at `CAL_WIDTH x CAL_HEIGHT` and the
/// window bounds clip the composition, so the content stays laid out
/// against the fixed `CAL_HEIGHT` while the window grows over it.
fn apply_reveal(hwnd: HWND, progress: f32) {
    use windows::Win32::UI::WindowsAndMessaging::{
        SetWindowPos, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOZORDER,
    };
    // SAFETY: plain query on a live window.
    let dpi = unsafe { GetDpiForWindow(hwnd).max(96) };
    let full = scaled(CAL_HEIGHT, dpi);
    let width = scaled(CAL_WIDTH, dpi);
    let height = MOTION.with(|m| m.borrow().reveal_extent(progress, full));
    // SAFETY: `hwnd` is a live, process-lifetime window; a zero height is
    // legal and the window is hidden once the phase reaches `Hidden`.
    unsafe {
        let _ = SetWindowPos(
            hwnd,
            None,
            0,
            0,
            width,
            height.max(0),
            SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
        );
    }
}

/// Advances the reveal one frame. Returns once the flyout has settled, so
/// the caller can drop the timer and hide the window.
pub(crate) fn tick(hwnd: HWND) {
    use windows::Win32::UI::WindowsAndMessaging::{KillTimer, ShowWindow, SW_HIDE};
    let progress = MOTION.with(|m| {
        let mut m = m.borrow_mut();
        if !m.is_animating() {
            return None;
        }
        Some(m.tick(std::time::Instant::now()))
    });
    if let Some(p) = progress {
        apply_reveal(hwnd, p);
    }
    if MOTION.with(|m| !m.borrow().is_visible()) {
        // SAFETY: `hwnd` is a live, process-lifetime window.
        unsafe {
            let _ = KillTimer(hwnd, CAL_TIMER_ID);
            let _ = ShowWindow(hwnd, SW_HIDE);
        }
    }
}

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
        // Roll back up rather than vanishing; `tick` hides the window once
        // the phase settles on `Hidden`. Under reduced motion the close is
        // instant and the window is hidden on the next tick immediately.
        MOTION.with(|m| m.borrow_mut().close());
        SetTimer(hwnd, CAL_TIMER_ID, super::design::motion::FRAME_INTERVAL_MS, None);
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
    // Always open on today, never on wherever the user last browsed to.
    MONTH_OFFSET.with(|m| m.set(0));

    // SAFETY: no preconditions.
    let previous_foreground = unsafe { windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow() };
    STATE.with(|s| {
        if let Some(state) = s.borrow_mut().as_mut() {
            state.previous_foreground = previous_foreground;
            state.calendar_open = true;
        }
    });

    if super::gpu::is_enabled() {
        STATE.with(|s| {
            let state = s.borrow();
            let Some(state) = state.as_ref() else { return };
            let Some(surface) = state.calendar_gpu.as_ref() else { return };
            let dpi = unsafe { GetDpiForWindow(hwnd).max(96) };
            super::gpu::redraw(surface, |ctx| paint_calendar_gpu(ctx, dpi));
        });
    }

    // SAFETY: `hwnd` is a valid, process-lifetime window.
    unsafe {
        if !super::gpu::is_enabled() {
            let _ = InvalidateRect(hwnd, None, true);
        }
        MOTION.with(|m| m.borrow_mut().open());
        apply_reveal(hwnd, 0.0);
        SetTimer(hwnd, CAL_TIMER_ID, super::design::motion::FRAME_INTERVAL_MS, None);
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

#[cfg(test)]
mod month_tests {
    use super::*;

    /// October 2026 starts on a Thursday — the month the flyout was
    /// first built against, and the anchor every other case walks from.
    const OCT_2026_FIRST_DOW: i32 = 4;

    #[test]
    fn the_anchor_month_keeps_its_own_first_weekday() {
        assert_eq!(
            first_dow_of(2026, 10, OCT_2026_FIRST_DOW, 2026, 10),
            OCT_2026_FIRST_DOW
        );
    }

    #[test]
    fn walking_forward_matches_the_real_calendar() {
        // 1 Nov 2026 is a Sunday, 1 Dec 2026 a Tuesday, 1 Jan 2027 a Friday.
        assert_eq!(first_dow_of(2026, 10, OCT_2026_FIRST_DOW, 2026, 11), 0);
        assert_eq!(first_dow_of(2026, 10, OCT_2026_FIRST_DOW, 2026, 12), 2);
        assert_eq!(first_dow_of(2026, 10, OCT_2026_FIRST_DOW, 2027, 1), 5);
    }

    #[test]
    fn walking_back_matches_the_real_calendar() {
        // 1 Sep 2026 is a Tuesday, 1 Aug 2026 a Saturday, 1 Jan 2026 a Thursday.
        assert_eq!(first_dow_of(2026, 10, OCT_2026_FIRST_DOW, 2026, 9), 2);
        assert_eq!(first_dow_of(2026, 10, OCT_2026_FIRST_DOW, 2026, 8), 6);
        assert_eq!(first_dow_of(2026, 10, OCT_2026_FIRST_DOW, 2026, 1), 4);
    }

    #[test]
    fn a_leap_february_shifts_the_following_month_correctly() {
        // 2028 is a leap year: 1 Feb 2028 is a Tuesday, 1 Mar 2028 a Wednesday.
        let feb = first_dow_of(2026, 10, OCT_2026_FIRST_DOW, 2028, 2);
        let mar = first_dow_of(2026, 10, OCT_2026_FIRST_DOW, 2028, 3);
        assert_eq!(feb, 2);
        assert_eq!(mar, 3, "29 days in Feb 2028 must push March on by one");
    }

    #[test]
    fn every_month_in_a_decade_lands_on_a_real_weekday() {
        for offset in -60..=60 {
            let (y, m) = shift_month(2026, 10, offset);
            let dow = first_dow_of(2026, 10, OCT_2026_FIRST_DOW, y, m);
            assert!((0..7).contains(&dow), "{y}-{m} gave weekday {dow}");
        }
    }

    #[test]
    fn a_zero_offset_is_the_month_you_are_in() {
        assert_eq!(shift_month(2026, 10, 0), (2026, 10));
    }

    #[test]
    fn stepping_forward_rolls_into_the_next_year() {
        assert_eq!(shift_month(2026, 12, 1), (2027, 1));
        assert_eq!(shift_month(2026, 11, 3), (2027, 2));
    }

    #[test]
    fn stepping_back_rolls_into_the_previous_year() {
        assert_eq!(shift_month(2026, 1, -1), (2025, 12));
        assert_eq!(shift_month(2026, 2, -14), (2024, 12));
    }

    #[test]
    fn a_full_year_either_way_returns_the_same_month() {
        for month in 1..=12 {
            assert_eq!(shift_month(2026, month, 12), (2027, month));
            assert_eq!(shift_month(2026, month, -12), (2025, month));
        }
    }

    #[test]
    fn every_offset_lands_on_a_real_month() {
        for offset in -40..=40 {
            let (_, month) = shift_month(2026, 6, offset);
            assert!((1..=12).contains(&month), "offset {offset} produced month {month}");
        }
    }
}

#[cfg(test)]
mod layout_tests {
    use super::*;

    fn all_cells(l: &CalLayout, first_dow: i32, days: i32) -> Vec<RECT> {
        (1..=days).map(|d| day_cell(l, first_dow, d)).collect()
    }

    #[test]
    fn the_first_of_the_month_lands_in_its_weekday_column() {
        let l = cal_layout(96);
        for first_dow in 0..7 {
            let cell = day_cell(&l, first_dow, 1);
            let expected = l.weekdays[first_dow as usize];
            assert_eq!(cell.left, expected.left, "day 1 misaligned for first_dow {first_dow}");
        }
    }

    #[test]
    fn day_cells_never_overlap() {
        let l = cal_layout(96);
        let cells = all_cells(&l, 6, 31);
        for (i, a) in cells.iter().enumerate() {
            for b in cells.iter().skip(i + 1) {
                let disjoint = a.right <= b.left || b.right <= a.left || a.bottom <= b.top || b.bottom <= a.top;
                assert!(disjoint, "cells overlap: {a:?} and {b:?}");
            }
        }
    }

    #[test]
    fn a_six_row_month_still_fits_inside_the_calendar_area() {
        // 31 days starting on a Saturday needs six rows - the worst case,
        // and the one a five-row grid would clip.
        let l = cal_layout(96);
        let last = day_cell(&l, 6, 31);
        assert!(
            last.bottom <= l.notif_header.top,
            "six-row month runs into the notifications section: {last:?} vs {:?}",
            l.notif_header
        );
    }

    #[test]
    fn every_cell_sits_within_the_cards_padding() {
        let l = cal_layout(96);
        for cell in all_cells(&l, 3, 31) {
            assert!(cell.left >= l.card.left, "{cell:?} escapes left");
            assert!(cell.right <= l.card.right, "{cell:?} escapes right");
        }
    }

    #[test]
    fn the_layout_scales_with_dpi() {
        let at96 = cal_layout(96);
        let at192 = cal_layout(192);
        assert_eq!(at192.card.right, at96.card.right * 2);
        assert_eq!(at192.cell_height, at96.cell_height * 2);
        let a = day_cell(&at96, 2, 15);
        let b = day_cell(&at192, 2, 15);
        assert_eq!(b.top, a.top * 2);
    }

    #[test]
    fn the_month_header_leaves_room_for_both_chevrons() {
        let l = cal_layout(96);
        assert!(l.prev.right <= l.header.left, "prev chevron overlaps the title");
        assert!(l.header.right <= l.next.left, "title overlaps the next chevron");
        assert!(l.next.right <= l.card.right);
    }

    #[test]
    fn the_notifications_section_is_below_the_grid_and_inside_the_card() {
        let l = cal_layout(96);
        assert!(l.notif_header.top >= l.weekdays[0].bottom);
        assert!(l.notif_body.top >= l.notif_header.bottom);
        assert!(l.notif_body.bottom <= l.card.bottom);
    }
}
