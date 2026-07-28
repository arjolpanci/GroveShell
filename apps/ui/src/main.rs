//! `groveshell-ui`: first-iteration shell UI. Per `docs/PROJECT_PLAN.md`
//! §10.1/§10.2, creates **one top bar per monitor** (Phase 4: "Create
//! per-monitor top bar windows and reserve working area") — only the
//! primary monitor's bar carries the three interactive regions (Activities,
//! clock, Quick Settings); other monitors just get an empty reserved strip
//! for now. Three flyouts:
//!
//! - **Activities overview**: spans the full virtual screen (every
//!   monitor), so a window on a non-primary monitor is fully inside its
//!   bounds and clickable — it previously only spanned the primary
//!   monitor, which clipped/broke windows on any other monitor. Each
//!   monitor gets its own "workspace card": a fixed-size rectangle (its
//!   desktop area below its own bar) that shrinks toward *its own* center,
//!   with that monitor's window thumbnails scaling in lockstep with it —
//!   not one shared pivot for the whole multi-monitor span, which would
//!   make two monitors' windows collide toward a single point between
//!   them. The card itself is drawn as a solid rect so a workspace's
//!   boundary stays visible even when a window inside it is small (the
//!   real desktop wallpaper isn't captured — see `paint_overview`).
//!   Clicking a thumbnail reverses the animation back to that window's
//!   real position and focuses it.
//! - **Calendar + notifications**: clicking the clock opens a Windows-11-style
//!   flyout centered below it — a real month calendar (today highlighted)
//!   stacked over a notifications section. The notifications section is a
//!   static "No new notifications" placeholder: reading the real
//!   notification feed requires `UserNotificationListener`, which needs a
//!   packaged app identity and explicit user consent that this unpackaged
//!   process doesn't have. Not faked beyond that empty-state text.
//! - **Quick Settings**: clicking the right side opens a flyout below it
//!   with real, working volume control (`IAudioEndpointVolume` — up/down/
//!   mute) and a read-only battery/AC status line. There's no icon tray:
//!   hosting other apps' notification-area icons would mean impersonating
//!   Explorer's own tray window, which is out of scope until Phase 7
//!   (Explorer replacement) per `docs/PROJECT_PLAN.md` §7 and ADR-002.
//!
//! Only one flyout is ever open at a time; opening any of the three closes
//! the other two. Each bar reserves its strip of its own monitor's work
//! area via the AppBar API (`SHAppBarMessage`), the same mechanism the
//! Windows taskbar uses, so maximized windows and desktop icon layout
//! respect it instead of being covered.
//!
//! ## Workspaces (Phase 3 + part of Phase 5)
//!
//! This is `groveshell-window-model`'s `ManagedWorkspaceBackend` (ADR-005),
//! with each currently-connected *monitor* additionally getting its own
//! fixed, always-present workspace ("pinned" — see
//! `groveshell_window_model::workspace`), ordered left-to-right by real
//! screen position, plus the standard GNOME-style dynamic tail (starts at
//! one extra empty workspace, grows/shrinks per §8.3) beyond the monitors.
//! A monitor's own workspace is never really "switched away from" in the
//! hide/show sense — its windows are already, always visible on that real
//! screen — so `commit_workspace_switch` only ever hides/shows windows
//! assigned to the dynamic (non-monitor) tail. "Current" mostly matters for
//! which page the overview centers on and where a moved window ends up.
//!
//! The Activities overview is a GNOME-style *carousel*: one fixed-size card
//! per workspace, wallpaper-filled, laid out side by side so the current
//! one is centered with previous/next cards peeking at the edges (see
//! `card_layout`); dragging horizontally slides between them (see
//! `carousel_offset`/`CarouselDrag`/`CarouselAnim`). A workspace's open
//! windows are arranged in a simple auto grid *within* its card — not at
//! their real screen position/size — each with a small app-icon badge
//! below it (see `layout_grid`). Every page's grid slots get live DWM
//! thumbnails: windows on an inactive workspace are *parked off-screen*
//! rather than hidden (see `park_window`), precisely because DWM renders
//! nothing for a hidden window's thumbnail; a title-only placeholder chip
//! remains only as the fallback when thumbnail registration fails.
//! Global hotkeys (`Ctrl+Alt+Left/Right` to switch, `+Shift` to also move
//! the focused window) work whether or not the overview is open. The top
//! bar stays visually on top of the overview (`SetWindowPos`
//! `HWND_TOPMOST` reassertion — see `open_overview`), and opening/closing
//! combines a whole-window fade (`SetLayeredWindowAttributes`) with a
//! GNOME-style zoom: the current workspace's card starts blown up to
//! roughly monitor size and shrinks into its carousel slot on open, and
//! the selected card grows back out on close (see `OVERVIEW_ZOOM_MAX` and
//! the `zoom` transform in `paint_overview`).
//!
//! Deliberately out of scope for this slice: *per-monitor sets of virtual*
//! workspaces (every monitor currently shares the one dynamic tail), moving
//! a window directly between two monitors' pages (only "pinned page ->
//! first dynamic page" is well-defined — see `move_focused_window_relative`),
//! Windows' own Task View virtual desktops (a separate, unrelated mechanism
//! — DWM already cloaks windows on an inactive one, and this shell's
//! cloaked-window filter already excludes those from every workspace's
//! page), hot corners, per-monitor DPI scaling, live wallpaper-change
//! detection (loaded once per process), and live updates while the
//! overview is open (a window opening/closing mid-session isn't picked up
//! until the overview is reopened).

#[cfg(windows)]
mod imp {
    use std::cell::RefCell;
    use std::ffi::c_void;
    use std::time::{Duration, Instant};

    use groveshell_common::{Error, Result};
    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::{BOOL, COLORREF, HWND, LPARAM, LRESULT, RECT, TRUE, WPARAM};
    use windows::Win32::Graphics::Gdi::{
        BeginPaint, BitBlt, CombineRgn, CreateCompatibleBitmap, CreateCompatibleDC, CreateFontW,
        CreateRectRgn, CreateRoundRectRgn, CreateSolidBrush, DeleteDC, DeleteObject, DrawTextW,
        Ellipse, EndPaint, EnumDisplayMonitors, FillRect, GetDC, GetMonitorInfoW, GetStockObject,
        InvalidateRect, ReleaseDC, RoundRect, SelectClipRgn, SelectObject, SetBkMode,
        SetStretchBltMode, SetTextColor, SetWindowRgn, StretchBlt, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS,
        CreatePen, DEFAULT_CHARSET, DEFAULT_PITCH, DRAW_TEXT_FORMAT, DT_CENTER, DT_END_ELLIPSIS,
        DT_SINGLELINE, DT_VCENTER, HALFTONE, HBITMAP, HDC, HMONITOR, HOLLOW_BRUSH, MONITORINFO,
        NULL_PEN, OUT_DEFAULT_PRECIS, PAINTSTRUCT, PS_SOLID, RGN_OR, SRCCOPY, TRANSPARENT,
    };
    use windows::Win32::Graphics::GdiPlus::{
        GdipCreateBitmapFromFile, GdipCreateFromHDC, GdipDeleteGraphics, GdipDrawImageRectI,
        GdiplusStartup, GdiplusStartupInput, GpBitmap, GpGraphics, GpImage,
    };
    use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
    use windows::Win32::Media::Audio::{eConsole, eRender, IMMDeviceEnumerator, MMDeviceEnumerator};
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_APARTMENTTHREADED};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS};
    use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
    use windows::Win32::System::SystemInformation::GetLocalTime;
    use windows::Win32::System::SystemServices::MK_LBUTTON;
    use windows::Win32::UI::HiDpi::{
        GetDpiForMonitor, GetDpiForWindow, SetProcessDpiAwarenessContext,
        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, MDT_EFFECTIVE_DPI,
    };
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        RegisterHotKey, SetFocus, UnregisterHotKey, MOD_ALT, MOD_CONTROL, MOD_SHIFT, VK_ESCAPE,
        VK_LEFT, VK_RIGHT,
    };
    use windows::Win32::UI::Shell::{
        SHAppBarMessage, ABE_TOP, ABM_GETSTATE, ABM_NEW, ABM_QUERYPOS, ABM_REMOVE, ABM_SETPOS,
        ABM_SETSTATE, ABS_AUTOHIDE, APPBARDATA,
    };
    use windows::Win32::UI::WindowsAndMessaging::*;

    use groveshell_window_model::workspace::{WorkspaceId, WorkspaceTracker};

    /// Half of the original 32px guess — the Windows taskbar itself is
    /// ~40px, but this shell is meant to feel closer to GNOME's slim bar;
    /// 50% taller than the first cut (16px) to fit the clock and quick
    /// settings labels comfortably.
    ///
    /// Like every other layout constant in this module, this is a *96-DPI*
    /// value: the process is per-monitor-DPI-aware, so all coordinates are
    /// physical pixels and every use site must pass through [`scaled`]
    /// with the relevant monitor/window DPI, or the bar comes out
    /// physically 24px tall — visibly tiny on a 125%/150% laptop panel.
    const BAR_HEIGHT: i32 = 24;
    /// Hit-test region for the painted (not native controls — there isn't
    /// enough vertical room in the bar for real button chrome) bar labels.
    const ACTIVITIES_LABEL_X: i32 = 8;
    const ACTIVITIES_LABEL_WIDTH: i32 = 72;
    const CLOCK_LABEL_WIDTH: i32 = 130;
    const QS_LABEL_WIDTH: i32 = 170;
    const QS_LABEL_MARGIN: i32 = 8;

    const ANIM_DURATION: Duration = Duration::from_millis(250);
    const ANIM_TIMER_ID: usize = 1;
    const ANIM_TIMER_INTERVAL_MS: u32 = 16;
    /// Refreshes the primary bar's clock text once a second.
    const CLOCK_TIMER_ID: usize = 2;

    const CAL_WIDTH: i32 = 320;
    const CAL_CALENDAR_HEIGHT: i32 = 300;
    const CAL_NOTIF_HEIGHT: i32 = 140;
    const CAL_HEIGHT: i32 = CAL_CALENDAR_HEIGHT + CAL_NOTIF_HEIGHT;
    const CAL_PADDING: i32 = 12;
    const CAL_CELL_HEIGHT: i32 = 34;

    const QS_WIDTH: i32 = 280;
    const QS_HEIGHT: i32 = 170;
    const QS_PADDING: i32 = 16;
    const QS_VOL_DOWN: i32 = 2001;
    const QS_VOL_UP: i32 = 2002;
    const QS_MUTE: i32 = 2003;

    const HOTKEY_WS_PREV: i32 = 3001;
    const HOTKEY_WS_NEXT: i32 = 3002;
    const HOTKEY_MOVE_WIN_PREV: i32 = 3003;
    const HOTKEY_MOVE_WIN_NEXT: i32 = 3004;

    /// Bar-side workspace indicator: a row of small dots to the right of
    /// "Activities," current one filled, the rest outlined.
    const WS_DOTS_X: i32 = ACTIVITIES_LABEL_X + ACTIVITIES_LABEL_WIDTH + 8;
    const WS_DOT_SLOT_WIDTH: i32 = 14;
    const WS_DOT_RADIUS: i32 = 3;

    /// Below this many pixels of horizontal movement, a press-and-release
    /// in the overview is treated as a click (focus/cancel/jump-to-page)
    /// rather than a carousel drag.
    const CAROUSEL_DRAG_CLICK_THRESHOLD_PX: i32 = 6;
    /// How long a released drag takes to snap to the nearest workspace page,
    /// or a keyboard/hotkey-triggered switch takes to slide into view.
    const CAROUSEL_SNAP_DURATION: Duration = Duration::from_millis(220);
    /// How many pixels of pointer movement it takes to drag the carousel by
    /// one full page. Deliberately independent of the actual page-to-page
    /// pixel pitch (`card_layout`'s card width can be over a thousand
    /// pixels on a large monitor) — this is a fixed, comfortable swipe
    /// distance instead, same idea as a touch or trackpad paging gesture.
    const CAROUSEL_DRAG_PAGE_DISTANCE_PX: f64 = 480.0;

    /// A workspace card's width as a fraction of the overview's full
    /// (virtual-screen) width — GNOME-style: big enough to read
    /// comfortably, small enough that the previous/next cards visibly peek
    /// in at the edges.
    const CARD_WIDTH_FRACTION: f64 = 0.62;
    /// Gap above a card, below the top bar — room for a future search box.
    const CARD_MARGIN_TOP: i32 = 70;
    /// Gap below a card — room for the future dock (not implemented yet).
    const CARD_MARGIN_BOTTOM: i32 = 120;
    /// Horizontal gap between adjacent cards; together with the card width
    /// this sets the carousel's page-to-page pitch.
    const CARD_GAP: i32 = 56;
    /// Inset from a card's edges to where its window grid starts.
    const CARD_CONTENT_PADDING: i32 = 28;
    /// Gap between grid cells.
    const THUMB_GAP: i32 = 20;
    const THUMB_ICON_SIZE: i32 = 28;

    /// How much a card (and everything on it) shrinks as it moves away
    /// from the carousel focus, as a fraction of full size — the focused
    /// workspace reads slightly larger than its neighbors, GNOME-style.
    /// Driven continuously by `carousel_offset`, so the size change eases
    /// in and out with the drag/slide instead of snapping.
    const CARD_UNFOCUS_SHRINK: f64 = 0.10;
    /// Corner radius of a workspace card (96-DPI, see [`scaled`]).
    const CARD_CORNER_RADIUS: i32 = 20;
    /// Corner radius of a window preview within a card (96-DPI).
    const THUMB_CORNER_RADIUS: i32 = 8;
    /// Rounded radius of the bar's *bottom* two corners (96-DPI); the top
    /// edge stays square against the screen edge.
    const BAR_CORNER_RADIUS: i32 = 10;
    /// The overview's zoom-out/zoom-in open/close animation runs between
    /// this scale (current card blown up to roughly monitor size —
    /// `1 / CARD_WIDTH_FRACTION` makes the card's width match the
    /// monitor's) and 1.0 (its normal carousel size).
    const OVERVIEW_ZOOM_MAX: f64 = 1.0 / CARD_WIDTH_FRACTION;

    #[derive(Clone, Copy)]
    struct MonitorInfo {
        /// Full monitor bounds in virtual-screen coordinates (which can be
        /// negative — the primary monitor anchors the origin, so a monitor
        /// to its left or above it has negative coordinates).
        rect: RECT,
        /// The monitor's work area as of enumeration — captured before
        /// this shell registers its own AppBars or hides the Windows
        /// taskbar, so it's exactly what must be restored at shutdown.
        work: RECT,
        is_primary: bool,
        /// Effective DPI of this monitor (96 = 100% scaling), used to
        /// convert this module's 96-DPI layout constants to physical
        /// pixels via [`scaled`].
        dpi: u32,
    }

    /// Converts a 96-DPI layout value to physical pixels at `dpi`,
    /// rounding to nearest.
    fn scaled(v: i32, dpi: u32) -> i32 {
        (v * dpi as i32 + 48) / 96
    }

    thread_local! {
        /// Every monitor's work area exactly as it was before this shell
        /// hid the Windows taskbar and claimed the screen — restored at
        /// shutdown so Explorer's taskbar gets its reservation back.
        static ORIGINAL_WORK_AREAS: RefCell<Vec<RECT>> = const { RefCell::new(Vec::new()) };
    }

    thread_local! {
        /// The taskbar's AppBar state (`ABM_GETSTATE`) before this shell
        /// switched it to auto-hide — restored at clean shutdown.
        static PREVIOUS_TASKBAR_STATE: RefCell<Option<u32>> = const { RefCell::new(None) };
    }

    fn taskbar_appbar_state() -> u32 {
        let mut abd = APPBARDATA {
            cbSize: std::mem::size_of::<APPBARDATA>() as u32,
            ..Default::default()
        };
        // SAFETY: plain query; `abd` outlives the call.
        (unsafe { SHAppBarMessage(ABM_GETSTATE, &mut abd) }) as u32
    }

    fn set_taskbar_appbar_state(state: u32) {
        let mut abd = APPBARDATA {
            cbSize: std::mem::size_of::<APPBARDATA>() as u32,
            lParam: LPARAM(state as isize),
            ..Default::default()
        };
        // SAFETY: plain state set; `abd` outlives the call.
        unsafe {
            SHAppBarMessage(ABM_SETSTATE, &mut abd);
        }
    }

    /// Takes the screen over from the Windows taskbar, or gives it back.
    ///
    /// Hiding the taskbar window alone isn't enough: its AppBar work-area
    /// reservation stays registered, so the strip it occupied remains dead
    /// space that maximized windows won't use, and Explorer re-asserts
    /// that reservation on any AppBar recalculation (which is also how an
    /// explicit `SPI_SETWORKAREA` kept getting reverted). Switching the
    /// taskbar to *auto-hide* state first (`ABM_SETSTATE`) makes Explorer
    /// itself release the reservation; the `SW_HIDE` on top keeps it from
    /// ever popping in. The pre-existing state is saved and restored at
    /// clean shutdown, and a 1-second watchdog re-hides it if Explorer
    /// re-shows it (see the `CLOCK_TIMER_ID` handler).
    fn set_windows_taskbar_visible(visible: bool) {
        if visible {
            if let Some(previous) = PREVIOUS_TASKBAR_STATE.with(|s| s.borrow_mut().take()) {
                set_taskbar_appbar_state(previous);
            }
        } else {
            PREVIOUS_TASKBAR_STATE.with(|s| {
                let mut slot = s.borrow_mut();
                if slot.is_none() {
                    *slot = Some(taskbar_appbar_state());
                }
            });
            set_taskbar_appbar_state(ABS_AUTOHIDE);
        }
        set_taskbar_windows_visible(visible);
    }

    /// Just the `ShowWindow` half of [`set_windows_taskbar_visible`] —
    /// also used by the periodic re-hide watchdog.
    fn set_taskbar_windows_visible(visible: bool) {
        let cmd = if visible { SW_SHOW } else { SW_HIDE };
        // SAFETY: plain window lookups/show-state changes on another
        // process's windows; all documented-fail harmlessly if Explorer
        // isn't running.
        unsafe {
            if let Ok(tray) = FindWindowW(w!("Shell_TrayWnd"), None) {
                let _ = ShowWindow(tray, cmd);
            }
            let mut previous = HWND(std::ptr::null_mut());
            while let Ok(next) =
                FindWindowExW(HWND(std::ptr::null_mut()), previous, w!("Shell_SecondaryTrayWnd"), None)
            {
                if next.0.is_null() {
                    break;
                }
                let _ = ShowWindow(next, cmd);
                previous = next;
            }
        }
    }

    /// Sets one monitor's work area (the rect maximized windows fill).
    /// `SPI_SETWORKAREA` applies to whichever monitor contains the rect.
    fn set_work_area(rect: RECT) {
        let mut rect = rect;
        // SAFETY: `rect` is a live local for the duration of the call.
        unsafe {
            let _ = SystemParametersInfoW(
                SPI_SETWORKAREA,
                0,
                Some(&mut rect as *mut RECT as *mut c_void),
                SPIF_SENDCHANGE,
            );
        }
    }

    /// With the taskbar hidden, its work-area reservation lingers — give
    /// every monitor's apps the full screen minus this shell's own bar.
    fn claim_work_areas(monitors: &[MonitorInfo]) {
        for monitor in monitors {
            set_work_area(RECT {
                left: monitor.rect.left,
                top: monitor.rect.top + scaled(BAR_HEIGHT, monitor.dpi),
                right: monitor.rect.right,
                bottom: monitor.rect.bottom,
            });
        }
    }

    fn restore_work_areas() {
        let areas = ORIGINAL_WORK_AREAS.with(|w| w.borrow().clone());
        for rect in areas {
            set_work_area(rect);
        }
    }

    /// Effective DPI of the primary monitor (falling back to the first
    /// enumerated one, then to 96) — the reference the overview's card
    /// layout is scaled against, matching `card_layout` basing the card
    /// geometry on the primary monitor.
    fn reference_dpi() -> u32 {
        let monitors = monitors_sorted_by_x();
        monitors
            .iter()
            .find(|m| m.is_primary)
            .or_else(|| monitors.first())
            .map(|m| m.dpi)
            .unwrap_or(96)
    }

    /// One monitor's top-level bar window plus the rect the AppBar system
    /// actually assigned it.
    struct BarWindow {
        hwnd: HWND,
        rect: RECT,
        is_primary: bool,
    }

    /// One window's grid slot within its workspace's card — a fixed,
    /// synthetic layout position (see `layout_grid`), *not* the window's
    /// real screen position/size. All rects are `overview_hwnd`-local,
    /// **page-local** (pre-carousel-shift) client-area coordinates — the
    /// horizontal carousel offset is applied on top of `rect`/`icon_rect`
    /// only at the moment they're pushed to DWM, painted, or hit-tested
    /// (see `displayed_rect`).
    struct ThumbAnim {
        hwnd: HWND,
        /// Used only for placeholder rendering.
        title: String,
        /// The window/app icon drawn at `icon_rect`, if one could be
        /// queried (`window_icon`) — owned by the window/class, never
        /// destroyed by this shell.
        icon: Option<HICON>,
        /// Which carousel page (workspace index, fixed for the lifetime of
        /// one overview session) this belongs to.
        page: usize,
        /// This window's grid-slot rect, aspect-fit within its cell.
        rect: RECT,
        /// Icon badge rect, directly below `rect`.
        icon_rect: RECT,
    }

    /// One workspace's card background rect — fixed once built (`page`
    /// only ever moves via the carousel shift, never animated in place;
    /// see the module docs on why the old "zoom from real position"
    /// animation no longer applies once windows are grid-arranged instead
    /// of position-accurate).
    struct CardAnim {
        page: usize,
        rect: RECT,
    }

    /// An in-progress pointer drag through the carousel, started on
    /// `WM_LBUTTONDOWN` while the overview is `Open`.
    struct CarouselDrag {
        start_x: i32,
        start_offset: f64,
        /// Furthest horizontal distance from `start_x` seen so far, used at
        /// release time to distinguish a drag from a plain click.
        max_delta: i32,
    }

    /// A smooth, non-interactive slide of `carousel_offset` toward a target
    /// page — used both for the drag-release "snap to nearest page" finish
    /// and for keyboard/hotkey-triggered switches.
    struct CarouselAnim {
        started: Instant,
        from: f64,
        to: f64,
    }

    enum OverviewMode {
        Closed,
        /// Visible but still fading in (`SetLayeredWindowAttributes`); the
        /// cards/thumbnails inside are already at their final layout the
        /// whole time — only the window's overall alpha animates.
        Opening {
            started: Instant,
            thumbs: Vec<ThumbAnim>,
            cards: Vec<CardAnim>,
        },
        /// Idle, fully visible; `thumbs[].rect`/`cards[].rect` are what
        /// clicks are hit-tested against (after applying the current
        /// carousel shift).
        Open {
            thumbs: Vec<ThumbAnim>,
            cards: Vec<CardAnim>,
        },
        /// Fading out; see `Opening`.
        Closing {
            started: Instant,
            thumbs: Vec<ThumbAnim>,
            cards: Vec<CardAnim>,
            /// `Some(hwnd)` when closing because a thumbnail was clicked
            /// (focus that window afterward); `None` when cancelled
            /// (Escape / empty-area click — restore whatever was focused
            /// before Activities was opened instead).
            focus_after: Option<HWND>,
        },
    }

    /// All mutable UI state lives here, on the single UI thread that owns
    /// every window created below. `thread_local!` (rather than a `static`)
    /// makes that single-thread assumption explicit and avoids `unsafe`
    /// globals for what is, in a classic Win32 app, thread-affine state
    /// anyway (window procedures for these windows only ever run on the
    /// thread that created them).
    struct AppState {
        bars: Vec<BarWindow>,
        /// Cached from `bars` for quick access — the only bar with
        /// Activities/clock/Quick Settings, and the one the calendar/QS
        /// flyouts are anchored under.
        primary_bar_hwnd: HWND,
        primary_bar_rect: RECT,
        overview_hwnd: HWND,
        calendar_hwnd: HWND,
        quick_settings_hwnd: HWND,
        overview: OverviewMode,
        calendar_open: bool,
        quick_settings_open: bool,
        /// Captured right before opening whichever flyout is currently
        /// open, so cancelling (Escape / empty-area click) can restore
        /// focus to whatever the user was actually doing. The bars
        /// themselves never become foreground (`WS_EX_NOACTIVATE`), so
        /// this is never just a bar.
        previous_foreground: HWND,
        /// Window→workspace assignment and the dynamic-workspace policy
        /// (see the module docs). Persists across overview open/close
        /// cycles; only the fields below are session-per-overview.
        workspaces: WorkspaceTracker,
        /// Current horizontal scroll position through the carousel, in page
        /// units (page index of whichever page is centered; fractional
        /// while dragging or mid-snap-animation). Only meaningful while the
        /// overview isn't `Closed`.
        carousel_offset: f64,
        carousel_drag: Option<CarouselDrag>,
        carousel_anim: Option<CarouselAnim>,
        /// Set when a carousel snap-animation should close the overview
        /// (focusing this window) once it lands, rather than just
        /// re-centering on the target page.
        carousel_close_after: Option<HWND>,
    }

    thread_local! {
        static STATE: RefCell<Option<AppState>> = const { RefCell::new(None) };
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Role {
        Bar { is_primary: bool },
        Overview,
        Calendar,
        QuickSettings,
        Other,
    }

    fn role_of(hwnd: HWND) -> Role {
        STATE.with(|s| {
            let state = s.borrow();
            let Some(st) = state.as_ref() else {
                return Role::Other;
            };
            if let Some(bar) = st.bars.iter().find(|b| b.hwnd == hwnd) {
                return Role::Bar {
                    is_primary: bar.is_primary,
                };
            }
            if hwnd == st.overview_hwnd {
                Role::Overview
            } else if hwnd == st.calendar_hwnd {
                Role::Calendar
            } else if hwnd == st.quick_settings_hwnd {
                Role::QuickSettings
            } else {
                Role::Other
            }
        })
    }

    /// Enumerates real monitors via `EnumDisplayMonitors`, falling back to
    /// a single synthetic monitor covering `GetSystemMetrics(SM_CXSCREEN
    /// /SM_CYSCREEN)` if that call somehow returns nothing (shouldn't
    /// happen on any real system, but every caller relies on this list
    /// being non-empty).
    fn enumerate_monitors() -> Vec<MonitorInfo> {
        let mut monitors: Vec<MonitorInfo> = Vec::new();
        // SAFETY: `monitors` is a local `Vec` whose address is passed
        // through as `lparam` and only read back by `monitor_enum_proc`
        // during this synchronous call.
        unsafe {
            let _ = EnumDisplayMonitors(
                None,
                None,
                Some(monitor_enum_proc),
                LPARAM(&mut monitors as *mut Vec<MonitorInfo> as isize),
            );
        }

        if monitors.is_empty() {
            // SAFETY: no preconditions; a plain metrics query.
            let (w, h) = unsafe { (GetSystemMetrics(SM_CXSCREEN), GetSystemMetrics(SM_CYSCREEN)) };
            let rect = RECT {
                left: 0,
                top: 0,
                right: w,
                bottom: h,
            };
            monitors.push(MonitorInfo {
                rect,
                work: rect,
                is_primary: true,
                dpi: 96,
            });
        }
        monitors
    }

    /// Monitors ordered left-to-right by real screen position — the order
    /// pinned monitor-workspaces are created in, so e.g. a laptop screen
    /// physically to the left of an external monitor becomes workspace 0
    /// with the external monitor as workspace 1, matching how the carousel
    /// then peeks left/right from whichever is current.
    fn monitors_sorted_by_x() -> Vec<MonitorInfo> {
        let mut monitors = enumerate_monitors();
        monitors.sort_by_key(|m| m.rect.left);
        monitors
    }

    /// Which of `monitors` contains the point `(center_x, center_y)`, if
    /// any — used to seed/assign a window to the workspace matching the
    /// real monitor it's actually on.
    fn monitor_index_for_center(monitors: &[MonitorInfo], center_x: i32, center_y: i32) -> Option<usize> {
        monitors.iter().position(|m| {
            center_x >= m.rect.left && center_x < m.rect.right && center_y >= m.rect.top && center_y < m.rect.bottom
        })
    }

    unsafe extern "system" fn monitor_enum_proc(
        hmonitor: HMONITOR,
        _hdc: HDC,
        _rect: *mut RECT,
        lparam: LPARAM,
    ) -> BOOL {
        // SAFETY: `lparam` was created from a live `&mut Vec<MonitorInfo>`
        // in `enumerate_monitors`, and this callback runs synchronously
        // within that call's lifetime.
        let monitors = &mut *(lparam.0 as *mut Vec<MonitorInfo>);

        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        if GetMonitorInfoW(hmonitor, &mut info).as_bool() {
            let (mut dpi_x, mut dpi_y) = (96u32, 96u32);
            let _ = GetDpiForMonitor(hmonitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y);
            monitors.push(MonitorInfo {
                rect: info.rcMonitor,
                work: info.rcWork,
                is_primary: info.dwFlags & MONITORINFOF_PRIMARY != 0,
                dpi: dpi_x.max(96),
            });
        }
        TRUE
    }

    pub fn main() -> Result<()> {
        // SAFETY: must run before any window is created or any DPI-
        // sensitive API is called (GetWindowRect, GetSystemMetrics,
        // EnumDisplayMonitors, DWM window attributes, ...) — this is the
        // very first thing `main` does. Without it, Windows silently
        // "DPI-virtualizes" coordinates for this process, and different
        // APIs virtualize inconsistently: `DwmGetWindowAttribute`'s
        // extended frame bounds come back in true physical pixels
        // regardless, while a plain `GetWindowRect` from a DPI-unaware
        // process gets scaled to look like 96 DPI. On a single monitor
        // both just happen to agree; on a mixed-DPI multi-monitor setup
        // they don't, and window-model's rect for anything on the
        // non-100%-scaled monitor comes out the wrong size/position
        // relative to everything else this process computes — this was
        // reproduced directly (a window measuring ~1129x635 via
        // `GetWindowRect` reported as ~2236x1259 here, matching a 200%
        // scale factor exactly) before adding this call.
        let _ = unsafe {
            SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)
        };

        let _log_guard = groveshell_common::logging::init("ui")?;
        tracing::info!("groveshell-ui starting");

        let _job = groveshell_common::jobobject::ShellJob::create_and_join()?;
        tracing::info!("joined shell job object");

        // SAFETY: called once, early, on the same single thread that makes
        // every other COM call in this process (volume control).
        // Initialization failure just means volume control degrades to
        // "unavailable" via the `Option`-returning helpers below — it's not
        // fatal to the rest of the shell.
        let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };

        // SAFETY: must run once, before any other `Gdip*` call — used to
        // draw the real wallpaper into overview cards (`draw_wallpaper_into`).
        // Never shut down: this process only ever exits via `PostQuitMessage`
        // and normal process teardown, same as the window classes registered
        // below are never unregistered.
        let mut gdiplus_token: usize = 0;
        let gdiplus_input = GdiplusStartupInput {
            GdiplusVersion: 1,
            ..Default::default()
        };
        let _ = unsafe { GdiplusStartup(&mut gdiplus_token, &gdiplus_input, std::ptr::null_mut()) };

        // SAFETY: every Win32 call below either has a call-site safety
        // comment or is a plain value/query with no aliasing or lifetime
        // requirements (e.g. `GetSystemMetrics`).
        unsafe {
            let hinstance = GetModuleHandleW(None).map_err(Error::Windows)?;
            let hinstance = windows::Win32::Foundation::HINSTANCE(hinstance.0);

            register_class(hinstance, w!("GroveShellBar"), Some(wndproc), 0x00202020)?;
            register_class(
                hinstance,
                w!("GroveShellOverview"),
                Some(wndproc),
                0x00404040,
            )?;
            register_class(
                hinstance,
                w!("GroveShellCalendar"),
                Some(wndproc),
                0x00303030,
            )?;
            register_class(
                hinstance,
                w!("GroveShellQuickSettings"),
                Some(wndproc),
                0x00303030,
            )?;

            // One bar per monitor (Phase 4). WS_EX_TOOLWINDOW keeps each
            // out of the taskbar/alt-tab and (as a side effect) out of its
            // own Activities listing, since `window-model::snapshot`
            // excludes tool windows. WS_EX_NOACTIVATE means clicking
            // anything on a bar never makes it the foreground window —
            // without it, `GetForegroundWindow()` when opening a flyout
            // would see the bar itself instead of whatever app the user
            // was actually using, breaking "restore focus on cancel."
            // Self-heal from a previous run that died without restoring
            // the taskbar (hard kill, crash): if it's hidden right now,
            // bring it back — and give Explorer a moment to re-reserve its
            // strip — *before* capturing "original" work areas below, or
            // this run would capture (and later dutifully restore) the
            // broken state.
            if let Ok(tray) = FindWindowW(w!("Shell_TrayWnd"), None) {
                if !IsWindowVisible(tray).as_bool() {
                    set_taskbar_appbar_state(0);
                    set_taskbar_windows_visible(true);
                    std::thread::sleep(Duration::from_millis(300));
                }
            }

            let monitors = enumerate_monitors();
            let mut bars = Vec::new();
            for monitor in &monitors {
                let width = monitor.rect.right - monitor.rect.left;
                let bar_height = scaled(BAR_HEIGHT, monitor.dpi);
                let bar_hwnd = CreateWindowExW(
                    WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                    w!("GroveShellBar"),
                    w!("GroveShell"),
                    WS_POPUP | WS_VISIBLE,
                    monitor.rect.left,
                    monitor.rect.top,
                    width,
                    bar_height,
                    None,
                    None,
                    hinstance,
                    None,
                )
                .map_err(Error::Windows)?;

                // Register this bar as a top-edge AppBar (the same
                // mechanism the Windows taskbar uses) so it reserves its
                // strip of *its own monitor's* work area — the AppBar API
                // has been monitor-aware since Windows 8, determined by
                // which monitor the given rect falls on.
                let bar_rect =
                    register_appbar(bar_hwnd, monitor.rect.left, monitor.rect.top, width, bar_height);
                let _ = MoveWindow(
                    bar_hwnd,
                    bar_rect.left,
                    bar_rect.top,
                    bar_rect.right - bar_rect.left,
                    bar_rect.bottom - bar_rect.top,
                    true,
                );

                // Round only the bar's *bottom* two corners: the window
                // region is a fully rounded rect unioned with a square
                // strip covering the top edge. `SetWindowRgn` takes
                // ownership of the region on success.
                let radius = scaled(BAR_CORNER_RADIUS, monitor.dpi);
                let region_w = bar_rect.right - bar_rect.left;
                let region_h = bar_rect.bottom - bar_rect.top;
                let region = CreateRoundRectRgn(0, 0, region_w + 1, region_h + 1, radius * 2, radius * 2);
                let top_square = CreateRectRgn(0, 0, region_w + 1, (region_h - radius).max(0));
                CombineRgn(region, region, top_square, RGN_OR);
                let _ = DeleteObject(top_square);
                SetWindowRgn(bar_hwnd, region, true);

                bars.push(BarWindow {
                    hwnd: bar_hwnd,
                    rect: bar_rect,
                    is_primary: monitor.is_primary,
                });
            }

            // This shell replaces the Windows taskbar while it runs: hide
            // it (and give apps its reserved strip back), remembering the
            // pre-existing work areas to restore at clean shutdown.
            ORIGINAL_WORK_AREAS.with(|w| *w.borrow_mut() = monitors.iter().map(|m| m.work).collect());
            set_windows_taskbar_visible(false);
            claim_work_areas(&monitors);

            let primary = bars.iter().find(|b| b.is_primary).unwrap_or(&bars[0]);
            let primary_bar_hwnd = primary.hwnd;
            let primary_bar_rect = primary.rect;

            // Overview: spans the full virtual screen (every monitor), not
            // just the primary one — a window on another monitor needs to
            // be fully inside the overview's bounds to be visible and
            // clickable at all.
            let virtual_x = GetSystemMetrics(SM_XVIRTUALSCREEN);
            let virtual_y = GetSystemMetrics(SM_YVIRTUALSCREEN);
            let virtual_w = GetSystemMetrics(SM_CXVIRTUALSCREEN);
            let virtual_h = GetSystemMetrics(SM_CYVIRTUALSCREEN);
            let overview_hwnd = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_LAYERED,
                w!("GroveShellOverview"),
                w!("GroveShell Activities"),
                WS_POPUP,
                virtual_x,
                virtual_y,
                virtual_w,
                virtual_h,
                None,
                None,
                hinstance,
                None,
            )
            .map_err(Error::Windows)?;

            // Calendar + notifications flyout, centered under the primary
            // bar's clock label, clamped so it never runs off that
            // monitor's edges.
            let primary_bar_width = primary_bar_rect.right - primary_bar_rect.left;
            let calendar_x = (primary_bar_rect.left + primary_bar_width / 2 - CAL_WIDTH / 2)
                .clamp(primary_bar_rect.left, (primary_bar_rect.right - CAL_WIDTH).max(primary_bar_rect.left));
            let calendar_hwnd = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
                w!("GroveShellCalendar"),
                w!("GroveShell Calendar"),
                WS_POPUP,
                calendar_x,
                primary_bar_rect.bottom,
                CAL_WIDTH,
                CAL_HEIGHT,
                None,
                None,
                hinstance,
                None,
            )
            .map_err(Error::Windows)?;

            // Quick Settings flyout, right-aligned under the primary bar's
            // right label.
            let qs_x = (primary_bar_rect.right - QS_WIDTH - QS_LABEL_MARGIN)
                .clamp(primary_bar_rect.left, (primary_bar_rect.right - QS_WIDTH).max(primary_bar_rect.left));
            let quick_settings_hwnd = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
                w!("GroveShellQuickSettings"),
                w!("GroveShell Quick Settings"),
                WS_POPUP,
                qs_x,
                primary_bar_rect.bottom,
                QS_WIDTH,
                QS_HEIGHT,
                None,
                None,
                hinstance,
                None,
            )
            .map_err(Error::Windows)?;

            CreateWindowExW(
                Default::default(),
                w!("BUTTON"),
                w!("-"),
                WS_CHILD | WS_VISIBLE | WINDOW_STYLE(BS_PUSHBUTTON as u32),
                QS_PADDING,
                90,
                40,
                28,
                quick_settings_hwnd,
                HMENU(QS_VOL_DOWN as *mut c_void),
                hinstance,
                None,
            )
            .map_err(Error::Windows)?;

            CreateWindowExW(
                Default::default(),
                w!("BUTTON"),
                w!("+"),
                WS_CHILD | WS_VISIBLE | WINDOW_STYLE(BS_PUSHBUTTON as u32),
                QS_PADDING + 48,
                90,
                40,
                28,
                quick_settings_hwnd,
                HMENU(QS_VOL_UP as *mut c_void),
                hinstance,
                None,
            )
            .map_err(Error::Windows)?;

            CreateWindowExW(
                Default::default(),
                w!("BUTTON"),
                w!("Mute"),
                WS_CHILD | WS_VISIBLE | WINDOW_STYLE(BS_PUSHBUTTON as u32),
                QS_WIDTH - QS_PADDING - 94,
                90,
                94,
                28,
                quick_settings_hwnd,
                HMENU(QS_MUTE as *mut c_void),
                hinstance,
                None,
            )
            .map_err(Error::Windows)?;

            let bar_hwnds: Vec<HWND> = bars.iter().map(|b| b.hwnd).collect();

            // One pinned workspace per currently-connected monitor, ordered
            // left-to-right by real screen position, with the primary
            // monitor's slot starting as current — see the module docs.
            // Seed each with whatever windows are already on that monitor
            // right now so a fresh launch doesn't show every open window
            // crammed onto workspace 0.
            let sorted_monitors = monitors_sorted_by_x();
            let primary_index = sorted_monitors.iter().position(|m| m.is_primary).unwrap_or(0);
            let mut workspaces = WorkspaceTracker::with_monitor_workspaces(sorted_monitors.len(), primary_index);
            for window in groveshell_window_model::snapshot() {
                let center_x = (window.rect.left + window.rect.right) / 2;
                let center_y = (window.rect.top + window.rect.bottom) / 2;
                let index = monitor_index_for_center(&sorted_monitors, center_x, center_y).unwrap_or(primary_index);
                workspaces.assign_to_index(window.hwnd, index);
            }

            STATE.with(|s| {
                *s.borrow_mut() = Some(AppState {
                    bars,
                    primary_bar_hwnd,
                    primary_bar_rect,
                    overview_hwnd,
                    calendar_hwnd,
                    quick_settings_hwnd,
                    overview: OverviewMode::Closed,
                    calendar_open: false,
                    quick_settings_open: false,
                    previous_foreground: HWND(std::ptr::null_mut()),
                    workspaces,
                    carousel_offset: 0.0,
                    carousel_drag: None,
                    carousel_anim: None,
                    carousel_close_after: None,
                });
            });

            // A `MoveWindow` above may have already triggered and consumed
            // a `WM_PAINT` for a bar before `STATE` existed, in which case
            // `wndproc` fell back to `DefWindowProcW` and the primary bar's
            // labels never actually got drawn. Force one more repaint on
            // every bar now that `STATE` is ready so they always show up,
            // regardless of how that first paint landed.
            for bar_hwnd in bar_hwnds {
                let _ = InvalidateRect(bar_hwnd, None, true);
            }
            SetTimer(primary_bar_hwnd, CLOCK_TIMER_ID, 1000, None);

            // Global workspace-switching hotkeys, delivered as `WM_HOTKEY`
            // to whichever window registered them — the primary bar, since
            // it already has a message loop and outlives every flyout.
            // Best-effort: another app may already own one of these
            // combinations, in which case that one shortcut silently
            // doesn't fire rather than failing shell startup.
            let _ = RegisterHotKey(primary_bar_hwnd, HOTKEY_WS_PREV, MOD_CONTROL | MOD_ALT, VK_LEFT.0 as u32);
            let _ = RegisterHotKey(primary_bar_hwnd, HOTKEY_WS_NEXT, MOD_CONTROL | MOD_ALT, VK_RIGHT.0 as u32);
            let _ = RegisterHotKey(
                primary_bar_hwnd,
                HOTKEY_MOVE_WIN_PREV,
                MOD_CONTROL | MOD_ALT | MOD_SHIFT,
                VK_LEFT.0 as u32,
            );
            let _ = RegisterHotKey(
                primary_bar_hwnd,
                HOTKEY_MOVE_WIN_NEXT,
                MOD_CONTROL | MOD_ALT | MOD_SHIFT,
                VK_RIGHT.0 as u32,
            );

            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }

        Ok(())
    }

    /// Registers `bar_hwnd` as a top-edge AppBar and reserves a
    /// `bar_height`-tall strip of the monitor at `(x, y)` for it, returning
    /// the rect the system assigned (per `ABM_SETPOS` semantics, this is
    /// what the caller should actually move/resize the window to). Every
    /// other top-level window's maximize/work-area layout on that monitor
    /// is recalculated by the system as a side effect, exactly as it is
    /// for the real taskbar.
    ///
    /// SAFETY: `bar_hwnd` must be a live window for the duration of this
    /// call; `SHAppBarMessage` only reads/writes through the `APPBARDATA`
    /// pointer for the duration of each call.
    unsafe fn register_appbar(bar_hwnd: HWND, x: i32, y: i32, width: i32, bar_height: i32) -> RECT {
        let mut abd = APPBARDATA {
            cbSize: std::mem::size_of::<APPBARDATA>() as u32,
            hWnd: bar_hwnd,
            ..Default::default()
        };
        SHAppBarMessage(ABM_NEW, &mut abd);

        abd.uEdge = ABE_TOP;
        abd.rc = RECT {
            left: x,
            top: y,
            right: x + width,
            bottom: y + bar_height,
        };
        // ABM_QUERYPOS lets other appbars adjust the proposed rect (e.g. if
        // the Windows taskbar already sits at the top of this monitor);
        // our height is fixed regardless, so only `bottom` is reasserted
        // afterward.
        SHAppBarMessage(ABM_QUERYPOS, &mut abd);
        abd.rc.bottom = abd.rc.top + bar_height;

        SHAppBarMessage(ABM_SETPOS, &mut abd);
        abd.rc
    }

    /// SAFETY: `bar_hwnd` was previously registered by [`register_appbar`];
    /// calling this after that registration is gone (e.g. twice) is a
    /// documented no-op on the shell's side, not undefined behavior.
    unsafe fn unregister_appbar(bar_hwnd: HWND) {
        let mut abd = APPBARDATA {
            cbSize: std::mem::size_of::<APPBARDATA>() as u32,
            hWnd: bar_hwnd,
            ..Default::default()
        };
        SHAppBarMessage(ABM_REMOVE, &mut abd);
    }

    /// SAFETY: `wndproc` must be a valid `WNDPROC`-compatible function
    /// pointer for the lifetime of the registered class, which holds for
    /// the whole process lifetime here (classes are never unregistered).
    unsafe fn register_class(
        hinstance: windows::Win32::Foundation::HINSTANCE,
        class_name: PCWSTR,
        wndproc: WNDPROC,
        background: u32,
    ) -> Result<()> {
        let class = WNDCLASSW {
            lpfnWndProc: wndproc,
            hInstance: hinstance,
            lpszClassName: class_name,
            hCursor: LoadCursorW(None, IDC_ARROW).map_err(Error::Windows)?,
            hbrBackground: CreateSolidBrush(COLORREF(background)),
            ..Default::default()
        };
        if RegisterClassW(&class) == 0 {
            return Err(Error::Windows(windows::core::Error::from_win32()));
        }
        Ok(())
    }

    /// A Segoe UI font sized for the bar at `dpi` (caller owns the handle
    /// and must `DeleteObject` it after deselecting).
    fn bar_font(dpi: u32) -> windows::Win32::Graphics::Gdi::HFONT {
        // SAFETY: plain object creation; no aliasing or lifetime
        // preconditions.
        unsafe {
            CreateFontW(
                -scaled(12, dpi),
                0,
                0,
                0,
                400,
                0,
                0,
                0,
                DEFAULT_CHARSET.0.into(),
                OUT_DEFAULT_PRECIS.0.into(),
                CLIP_DEFAULT_PRECIS.0.into(),
                CLEARTYPE_QUALITY.0.into(),
                DEFAULT_PITCH.0.into(),
                w!("Segoe UI"),
            )
        }
    }

    /// SAFETY: `hdc` must be a valid device context obtained from
    /// `BeginPaint` on the window currently handling `WM_PAINT`.
    unsafe fn draw_text_in(hdc: HDC, rect: RECT, text: &str, format: DRAW_TEXT_FORMAT) {
        let mut wide: Vec<u16> = text.encode_utf16().collect();
        let mut r = rect;
        DrawTextW(hdc, &mut wide, &mut r, format);
    }

    /// Paints a bar. Non-primary bars are just the plain class-brush
    /// background (still validated via `BeginPaint`/`EndPaint` so Windows
    /// doesn't keep re-queuing `WM_PAINT`) — only the primary monitor
    /// carries Activities/clock/Quick Settings (per
    /// `docs/PROJECT_PLAN.md` §10.1). There are no native `BUTTON`
    /// controls for these — at this bar height a real push button's chrome
    /// leaves no room for legible text, so this is flat painted text
    /// hit-tested in `WM_LBUTTONUP` instead (see `on_bar_click`).
    fn paint_bar(hwnd: HWND, is_primary: bool) {
        // SAFETY: `hwnd` is the window currently processing `WM_PAINT`, so
        // it's guaranteed valid for the duration of this call; `ps` is a
        // local that outlives the paired `BeginPaint`/`EndPaint` call.
        unsafe {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);

            if is_primary {
                let dpi = GetDpiForWindow(hwnd).max(96);
                let bar_h = scaled(BAR_HEIGHT, dpi);
                let bar_width = STATE
                    .with(|s| {
                        s.borrow()
                            .as_ref()
                            .map(|st| st.primary_bar_rect.right - st.primary_bar_rect.left)
                    })
                    .unwrap_or(0);

                SetBkMode(hdc, TRANSPARENT);
                SetTextColor(hdc, COLORREF(0x00E0E0E0));
                // The DC's default font is the fixed-size legacy "System"
                // font, which neither scales with DPI nor matches the rest
                // of the OS — use Segoe UI sized to the bar's monitor.
                let font = bar_font(dpi);
                let previous_font = SelectObject(hdc, font);

                let format = DT_SINGLELINE | DT_VCENTER | DT_CENTER;
                draw_text_in(
                    hdc,
                    RECT {
                        left: scaled(ACTIVITIES_LABEL_X, dpi),
                        top: 0,
                        right: scaled(ACTIVITIES_LABEL_X + ACTIVITIES_LABEL_WIDTH, dpi),
                        bottom: bar_h,
                    },
                    "Activities",
                    format,
                );

                let (workspace_count, current_index) = STATE
                    .with(|s| {
                        s.borrow()
                            .as_ref()
                            .map(|st| (st.workspaces.workspace_ids().len(), st.workspaces.current_index()))
                    })
                    .unwrap_or((0, 0));
                let dot_mid_y = bar_h / 2;
                let dot_slot_w = scaled(WS_DOT_SLOT_WIDTH, dpi);
                let dot_radius = scaled(WS_DOT_RADIUS, dpi);
                let filled_brush = CreateSolidBrush(COLORREF(0x00E0E0E0));
                let empty_brush = CreateSolidBrush(COLORREF(0x00606060));
                for i in 0..workspace_count {
                    let cx = scaled(WS_DOTS_X, dpi) + i as i32 * dot_slot_w + dot_slot_w / 2;
                    let brush = if i == current_index { filled_brush } else { empty_brush };
                    let previous = SelectObject(hdc, brush);
                    let _ = Ellipse(
                        hdc,
                        cx - dot_radius,
                        dot_mid_y - dot_radius,
                        cx + dot_radius,
                        dot_mid_y + dot_radius,
                    );
                    SelectObject(hdc, previous);
                }
                let _ = DeleteObject(filled_brush);
                let _ = DeleteObject(empty_brush);

                let clock_x = bar_width / 2 - scaled(CLOCK_LABEL_WIDTH, dpi) / 2;
                draw_text_in(
                    hdc,
                    RECT {
                        left: clock_x,
                        top: 0,
                        right: clock_x + scaled(CLOCK_LABEL_WIDTH, dpi),
                        bottom: bar_h,
                    },
                    &clock_text(),
                    format,
                );

                let qs_x = bar_width - scaled(QS_LABEL_WIDTH + QS_LABEL_MARGIN, dpi);
                draw_text_in(
                    hdc,
                    RECT {
                        left: qs_x,
                        top: 0,
                        right: qs_x + scaled(QS_LABEL_WIDTH, dpi),
                        bottom: bar_h,
                    },
                    &quick_settings_label_text(),
                    format,
                );

                SelectObject(hdc, previous_font);
                let _ = DeleteObject(font);
            }

            let _ = EndPaint(hwnd, &ps);
        }
    }

    fn clock_text() -> String {
        // SAFETY: no preconditions.
        let t = unsafe { GetLocalTime() };
        let hour12 = match t.wHour % 12 {
            0 => 12,
            h => h,
        };
        let ampm = if t.wHour < 12 { "AM" } else { "PM" };
        format!("{hour12:02}:{:02} {ampm}", t.wMinute)
    }

    fn quick_settings_label_text() -> String {
        match battery_percent() {
            Some(pct) => format!("{pct}%  Quick Settings"),
            None => "Quick Settings".to_string(),
        }
    }

    /// `None` when there's no battery to report (desktop on AC), not on
    /// I/O failure — `GetSystemPowerStatus` reports `255` for "unknown",
    /// which covers both cases; either way there's nothing meaningful to
    /// show.
    fn battery_percent() -> Option<u8> {
        // SAFETY: `status` is a local, zeroed `SYSTEM_POWER_STATUS` that
        // outlives this synchronous call.
        unsafe {
            let mut status = SYSTEM_POWER_STATUS::default();
            GetSystemPowerStatus(&mut status).ok()?;
            (status.BatteryLifePercent != 255).then_some(status.BatteryLifePercent)
        }
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
    fn paint_calendar(hwnd: HWND) {
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

    fn paint_quick_settings(hwnd: HWND) {
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

    /// Draws every workspace card (wallpaper-filled — see
    /// `draw_wallpaper_into`) at its current carousel-shifted position,
    /// then each window's grid-slot content: a placeholder chip with title
    /// for anything without a live thumbnail (DWM draws live ones directly,
    /// this process never touches their pixels), and every slot's icon
    /// badge regardless of live/placeholder.
    fn paint_overview(hwnd: HWND) {
        let content = STATE.with(|s| {
            let state = s.borrow();
            let st = state.as_ref()?;
            let (cards, thumbs) = match &st.overview {
                OverviewMode::Opening { cards, thumbs, .. }
                | OverviewMode::Open { cards, thumbs }
                | OverviewMode::Closing { cards, thumbs, .. } => (cards, thumbs),
                OverviewMode::Closed => return None,
            };
            let (card_rect, pitch) = card_layout();

            // Open/close zoom: everything scales about the focused card's
            // center, from "current card blown up to monitor size" at the
            // closed end of the animation to normal carousel size when
            // fully open. Fully `Open` paints with no zoom (s == 1).
            let zoom = match &st.overview {
                OverviewMode::Opening { started, .. } => {
                    let t = ease_out(progress(*started));
                    OVERVIEW_ZOOM_MAX + (1.0 - OVERVIEW_ZOOM_MAX) * t
                }
                OverviewMode::Closing { started, .. } => {
                    let t = ease_out(progress(*started));
                    1.0 + (OVERVIEW_ZOOM_MAX - 1.0) * t
                }
                _ => 1.0,
            };
            let anchor_x = (card_rect.left + card_rect.right) as f64 / 2.0;
            let anchor_y = (card_rect.top + card_rect.bottom) as f64 / 2.0;
            let place = |base: RECT, page: usize| {
                let r = displayed_rect(base, page, st.carousel_offset, pitch, card_rect);
                zoom_rect(r, anchor_x, anchor_y, zoom)
            };

            let cards = cards.iter().map(|c| place(c.rect, c.page)).collect::<Vec<_>>();
            // Every window preview is one of our snapshots (park-time for
            // parked windows, open-time for the current workspace's — see
            // `build_carousel_pages`); a title chip is the last-resort
            // fallback when no capture succeeded. Alongside the displayed
            // rect, each entry carries its *base* (untransformed) slot
            // size — the size the pre-scaled bitmap cache is keyed on.
            let mut snapshots: Vec<(RECT, i32, i32, isize)> = Vec::new();
            let mut placeholders: Vec<(RECT, String)> = Vec::new();
            for th in thumbs.iter() {
                let rect = place(th.rect, th.page);
                let hwnd = th.hwnd.0 as isize;
                if window_snapshot(hwnd).is_some() {
                    snapshots.push((rect, th.rect.right - th.rect.left, th.rect.bottom - th.rect.top, hwnd));
                } else {
                    placeholders.push((rect, th.title.clone()));
                }
            }
            let icons = thumbs
                .iter()
                .filter_map(|th| th.icon.map(|icon| (place(th.icon_rect, th.page), icon)))
                .collect::<Vec<_>>();
            Some((cards, snapshots, placeholders, icons))
        });

        // Double-buffered: everything is composed into a memory bitmap and
        // blitted to the screen in one `BitBlt`. Painting straight to the
        // window DC drew the backdrop, wallpaper, chips, and icons as
        // separately visible steps — flicker on every drag `WM_MOUSEMOVE`.
        // The paired `WM_ERASEBKGND` handler returns nonzero for this
        // window (see `wndproc`) since the backdrop fill happens here.
        //
        // SAFETY: `hwnd` is the window currently processing `WM_PAINT`;
        // all GDI objects created here are torn down before returning.
        unsafe {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);

            let mut client = RECT::default();
            let _ = GetClientRect(hwnd, &mut client);
            let (w, h) = (client.right.max(1), client.bottom.max(1));
            let mem = CreateCompatibleDC(hdc);
            let buffer = overview_back_buffer(hdc, w, h);
            let previous = SelectObject(mem, buffer);

            // Same backdrop color as the window class's brush.
            let backdrop_brush = CreateSolidBrush(COLORREF(0x00404040));
            FillRect(mem, &client, backdrop_brush);
            let _ = DeleteObject(backdrop_brush);

            if let Some((cards, snapshots, placeholders, icons)) = content {
                let dpi = reference_dpi();
                let card_radius = scaled(CARD_CORNER_RADIUS, dpi);
                let thumb_radius = scaled(THUMB_CORNER_RADIUS, dpi);
                // Cheap stretch mode for all per-frame scaling — sources
                // are already HALFTONE-prescaled to ~the right size.
                SetStretchBltMode(mem, windows::Win32::Graphics::Gdi::COLORONCOLOR);

                // Cards: drop shadow, then wallpaper clipped to a rounded
                // rect (flat fallback fill underneath in case the
                // wallpaper failed to load — a card should never look like
                // a hole in the UI).
                let fallback_brush = CreateSolidBrush(COLORREF(0x00302010));
                for rect in &cards {
                    draw_shadow(mem, *rect, card_radius, 6);
                    let clip = CreateRoundRectRgn(
                        rect.left,
                        rect.top,
                        rect.right + 1,
                        rect.bottom + 1,
                        card_radius * 2,
                        card_radius * 2,
                    );
                    SelectClipRgn(mem, clip);
                    FillRect(mem, rect, fallback_brush);
                    draw_wallpaper_into(mem, *rect);
                    SelectClipRgn(mem, windows::Win32::Graphics::Gdi::HRGN(std::ptr::null_mut()));
                    let _ = DeleteObject(clip);
                }
                let _ = DeleteObject(fallback_brush);

                // Window previews: shadow, then the pre-scaled snapshot
                // (see `slot_scaled_snapshot` — per-frame stretching of
                // the full-size captures was a large part of the lag)
                // into its (already aspect-fit — see `layout_grid`) slot,
                // clipped to a smaller rounded rect. `COLORONCOLOR` here:
                // the per-frame ratio is near 1:1, where it's visually
                // fine and much cheaper than HALFTONE.
                if !snapshots.is_empty() {
                    let src = CreateCompatibleDC(hdc);
                    SetStretchBltMode(mem, windows::Win32::Graphics::Gdi::COLORONCOLOR);
                    for (rect, base_w, base_h, hwnd) in &snapshots {
                        let Some(bitmap) = slot_scaled_snapshot(*hwnd, *base_w, *base_h) else {
                            continue;
                        };
                        draw_shadow(mem, *rect, thumb_radius, 4);
                        let clip = CreateRoundRectRgn(
                            rect.left,
                            rect.top,
                            rect.right + 1,
                            rect.bottom + 1,
                            thumb_radius * 2,
                            thumb_radius * 2,
                        );
                        SelectClipRgn(mem, clip);
                        let previous_src = SelectObject(src, HBITMAP(bitmap as *mut c_void));
                        let (dst_w, dst_h) = (rect.right - rect.left, rect.bottom - rect.top);
                        if dst_w == *base_w && dst_h == *base_h {
                            let _ = BitBlt(mem, rect.left, rect.top, dst_w, dst_h, src, 0, 0, SRCCOPY);
                        } else {
                            let _ = StretchBlt(
                                mem, rect.left, rect.top, dst_w, dst_h, src, 0, 0, *base_w, *base_h,
                                SRCCOPY,
                            );
                        }
                        SelectObject(src, previous_src);
                        SelectClipRgn(mem, windows::Win32::Graphics::Gdi::HRGN(std::ptr::null_mut()));
                        let _ = DeleteObject(clip);
                    }
                    let _ = DeleteDC(src);
                }

                // Placeholder chips: fallback for windows with no snapshot
                // (capture failed, window died, or it was minimized when
                // parked) — just their last-known title.
                let chip_brush = CreateSolidBrush(COLORREF(0x00303030));
                let null_pen = GetStockObject(NULL_PEN);
                SetBkMode(mem, TRANSPARENT);
                SetTextColor(mem, COLORREF(0x00E0E0E0));
                for (rect, title) in &placeholders {
                    draw_shadow(mem, *rect, thumb_radius, 4);
                    let previous_brush = SelectObject(mem, chip_brush);
                    let previous_pen = SelectObject(mem, null_pen);
                    let _ = RoundRect(
                        mem,
                        rect.left,
                        rect.top,
                        rect.right,
                        rect.bottom,
                        thumb_radius * 2,
                        thumb_radius * 2,
                    );
                    SelectObject(mem, previous_pen);
                    SelectObject(mem, previous_brush);
                    draw_text_in(
                        mem,
                        *rect,
                        title,
                        DT_SINGLELINE | DT_VCENTER | DT_CENTER | DT_END_ELLIPSIS,
                    );
                }
                let _ = DeleteObject(chip_brush);

                for (rect, icon) in &icons {
                    let size = rect.right - rect.left;
                    let _ = DrawIconEx(mem, rect.left, rect.top, *icon, size, size, 0, None, DI_NORMAL);
                }
            }

            let _ = BitBlt(hdc, 0, 0, w, h, mem, 0, 0, SRCCOPY);
            SelectObject(mem, previous);
            let _ = DeleteDC(mem);
            let _ = EndPaint(hwnd, &ps);
        }
    }

    /// Fakes a soft drop shadow under a rounded rect: concentric *hollow*
    /// rounded-rect rings (2px pens — the content drawn on top covers the
    /// interior anyway, and filling each layer repainted the whole card
    /// area several times per frame, a measurable part of the animation
    /// cost), biased a few pixels downward, stepping from the overview
    /// backdrop (0x404040) toward near-black as they close in on `rect`.
    /// GDI has no alpha blur; against the flat backdrop this layered
    /// approximation is indistinguishable at a glance. Must be drawn
    /// *before* the content that casts it.
    ///
    /// SAFETY: `hdc` must be a valid memory DC currently being painted into.
    unsafe fn draw_shadow(hdc: HDC, rect: RECT, radius: i32, layers: i32) {
        let hollow_brush = GetStockObject(HOLLOW_BRUSH);
        let previous_brush = SelectObject(hdc, hollow_brush);
        for i in 0..layers {
            // Outermost ring first (largest, faintest).
            let spread = layers - i;
            let t = (i + 1) as f64 / layers as f64;
            let channel = (0x40 as f64 + (0x1C as f64 - 0x40 as f64) * t).round() as u32;
            let pen = CreatePen(PS_SOLID, 2, COLORREF(channel | (channel << 8) | (channel << 16)));
            let previous_pen = SelectObject(hdc, pen);
            let _ = RoundRect(
                hdc,
                rect.left - spread,
                rect.top - spread + 3,
                rect.right + spread,
                rect.bottom + spread + 3,
                (radius + spread) * 2,
                (radius + spread) * 2,
            );
            SelectObject(hdc, previous_pen);
            let _ = DeleteObject(pen);
        }
        SelectObject(hdc, previous_brush);
    }

    thread_local! {
        /// The overview's reusable back buffer (`(width, height, HBITMAP)`).
        /// Allocating a fresh full-virtual-screen bitmap (tens of MB) on
        /// every `WM_PAINT` was measurable jank at drag-repaint rates;
        /// recreated only if the client size changes.
        static OVERVIEW_BACK_BUFFER: RefCell<Option<(i32, i32, isize)>> = const { RefCell::new(None) };
    }

    /// SAFETY: `hdc` must be a valid device context; the returned HBITMAP
    /// is owned by the cache and must not be deleted by the caller.
    unsafe fn overview_back_buffer(hdc: HDC, w: i32, h: i32) -> HBITMAP {
        if let Some((cw, ch, handle)) = OVERVIEW_BACK_BUFFER.with(|c| *c.borrow()) {
            if cw == w && ch == h {
                return HBITMAP(handle as *mut c_void);
            }
            let _ = DeleteObject(HBITMAP(handle as *mut c_void));
        }
        let buffer = CreateCompatibleBitmap(hdc, w, h);
        OVERVIEW_BACK_BUFFER.with(|c| *c.borrow_mut() = Some((w, h, buffer.0 as isize)));
        buffer
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

    fn get_volume_percent() -> Option<u32> {
        with_volume(|v| unsafe { v.GetMasterVolumeLevelScalar() })
            .map(|scalar| (scalar * 100.0).round() as u32)
    }

    fn get_mute() -> Option<bool> {
        with_volume(|v| unsafe { v.GetMute() }).map(|b| b.as_bool())
    }

    fn adjust_volume(delta_percent: i32) {
        let Some(current) = get_volume_percent() else {
            return;
        };
        let next = (current as i32 + delta_percent).clamp(0, 100) as f32 / 100.0;
        // SAFETY: no preconditions beyond `with_volume`'s own.
        let _ = with_volume(|v| unsafe { v.SetMasterVolumeLevelScalar(next, std::ptr::null()) });
    }

    fn toggle_mute() {
        let Some(muted) = get_mute() else {
            return;
        };
        // SAFETY: no preconditions beyond `with_volume`'s own.
        let _ = with_volume(|v| unsafe { v.SetMute(!muted, std::ptr::null()) });
    }

    fn ease_out(t: f64) -> f64 {
        1.0 - (1.0 - t).powi(3)
    }

    /// Where a page-local rect actually appears right now: translated by
    /// the carousel scroll (page `page` sits at `(page - offset) * pitch`
    /// pixels from its own local origin), then shrunk toward that page's
    /// card center by how far the page is from the carousel focus (see
    /// `CARD_UNFOCUS_SHRINK` — this is what makes the focused card bigger
    /// than its neighbors, smoothly, since it's a pure function of
    /// `offset`). Applied only at the moment a rect is painted or
    /// hit-tested — never baked back into `ThumbAnim`/`CardAnim::rect`.
    fn displayed_rect(base: RECT, page: usize, offset: f64, pitch: i32, card: RECT) -> RECT {
        let dx = (page as f64 - offset) * pitch as f64;
        let s = 1.0 - CARD_UNFOCUS_SHRINK * (page as f64 - offset).abs().min(1.0);
        let cx = (card.left + card.right) as f64 / 2.0 + dx;
        let cy = (card.top + card.bottom) as f64 / 2.0;
        let map_x = |x: i32| (cx + (x as f64 + dx - cx) * s).round() as i32;
        let map_y = |y: i32| (cy + (y as f64 - cy) * s).round() as i32;
        RECT {
            left: map_x(base.left),
            top: map_y(base.top),
            right: map_x(base.right),
            bottom: map_y(base.bottom),
        }
    }

    /// Scales `r` about `(anchor_x, anchor_y)` by `s` — the open/close
    /// zoom transform, applied on top of `displayed_rect` at paint time
    /// only (input never round-trips, so no drift).
    fn zoom_rect(r: RECT, anchor_x: f64, anchor_y: f64, s: f64) -> RECT {
        let map_x = |x: i32| (anchor_x + (x as f64 - anchor_x) * s).round() as i32;
        let map_y = |y: i32| (anchor_y + (y as f64 - anchor_y) * s).round() as i32;
        RECT {
            left: map_x(r.left),
            top: map_y(r.top),
            right: map_x(r.right),
            bottom: map_y(r.bottom),
        }
    }

    /// The fixed, page-local rect every workspace's card occupies (GNOME-
    /// style: a large card sized as a fraction of the *primary monitor's*
    /// width — not the full multi-monitor virtual screen, which on a
    /// multi-monitor setup is much wider than any one screen and would
    /// produce a card spanning most of both monitors — vertically centered
    /// in the space below the bar and above the future dock, horizontally
    /// centered on the primary monitor specifically so the focused card
    /// actually sits on the screen the user is looking at), plus the
    /// page-to-page pitch (card width + gap) the carousel spaces pages
    /// apart by. Recomputed on demand rather than cached: cheap, and
    /// consistent with the rest of this module not handling live
    /// display-topology changes.
    fn card_layout() -> (RECT, i32) {
        // SAFETY: no preconditions. The overview window's own top-left
        // sits at exactly this point (see `open_overview`), so subtracting
        // it converts a monitor's absolute virtual-screen rect into
        // overview-client-local coordinates.
        let (origin_x, client_h) = unsafe {
            (GetSystemMetrics(SM_XVIRTUALSCREEN), GetSystemMetrics(SM_CYVIRTUALSCREEN))
        };

        let monitors = monitors_sorted_by_x();
        let reference = monitors.iter().find(|m| m.is_primary).or_else(|| monitors.first());
        let (ref_w, ref_aspect, ref_center_x_abs, dpi) = match reference {
            Some(m) => {
                let w = (m.rect.right - m.rect.left) as f64;
                let h = (m.rect.bottom - m.rect.top).max(1) as f64;
                (w, w / h, (m.rect.left + m.rect.right) / 2, m.dpi)
            }
            None => (1920.0, 16.0 / 9.0, origin_x + 960, 96),
        };

        let card_w = (ref_w * CARD_WIDTH_FRACTION).round() as i32;
        let max_card_h =
            (client_h - scaled(BAR_HEIGHT + CARD_MARGIN_TOP + CARD_MARGIN_BOTTOM, dpi)).max(1);
        let card_h = ((card_w as f64 / ref_aspect).round() as i32).min(max_card_h).max(1);

        let card_top = scaled(BAR_HEIGHT + CARD_MARGIN_TOP, dpi) + (max_card_h - card_h) / 2;
        let card_left = (ref_center_x_abs - origin_x) - card_w / 2;
        let rect = RECT {
            left: card_left,
            top: card_top,
            right: card_left + card_w,
            bottom: card_top + card_h,
        };
        (rect, card_w + scaled(CARD_GAP, dpi))
    }

    /// Arranges `windows` in a simple auto grid within `card` — GNOME-
    /// style: their assigned slot, not their real screen position/size —
    /// each slot aspect-fit (letterboxed, not stretched/distorted) within
    /// its cell, with room left below for a small icon badge. Returns
    /// `(thumbnail_slot_rect, icon_rect, window)` triples in the same
    /// order as `windows`.
    fn layout_grid(
        card: RECT,
        windows: Vec<groveshell_window_model::WindowRecord>,
    ) -> Vec<(RECT, RECT, groveshell_window_model::WindowRecord)> {
        if windows.is_empty() {
            return Vec::new();
        }

        let dpi = reference_dpi();
        let padding = scaled(CARD_CONTENT_PADDING, dpi);
        let content = RECT {
            left: card.left + padding,
            top: card.top + padding,
            right: card.right - padding,
            bottom: card.bottom - padding,
        };
        let n = windows.len();
        let cols = (n as f64).sqrt().ceil().max(1.0) as i32;
        let rows = (n as i32 + cols - 1) / cols;

        let icon_size = scaled(THUMB_ICON_SIZE, dpi);
        let thumb_gap = scaled(THUMB_GAP, dpi);
        let cell_w = ((content.right - content.left).max(1)) / cols;
        let cell_h = ((content.bottom - content.top).max(1)) / rows;
        // The icon badge straddles the preview's bottom edge (centered on
        // it), so only its protruding lower half needs reserved room.
        let icon_band_h = icon_size / 2;
        let slot_w = (cell_w - thumb_gap).max(1);
        let slot_h = (cell_h - thumb_gap - icon_band_h).max(1);

        windows
            .into_iter()
            .enumerate()
            .map(|(i, window)| {
                let col = i as i32 % cols;
                let row = i as i32 / cols;
                let cell_left = content.left + col * cell_w;
                let cell_top = content.top + row * cell_h;

                let src_w = (window.rect.right - window.rect.left).max(1) as f64;
                let src_h = (window.rect.bottom - window.rect.top).max(1) as f64;
                let src_aspect = src_w / src_h;
                let slot_aspect = slot_w as f64 / slot_h as f64;
                let (fit_w, fit_h) = if src_aspect > slot_aspect {
                    (slot_w, ((slot_w as f64 / src_aspect).round() as i32).max(1))
                } else {
                    (((slot_h as f64 * src_aspect).round() as i32).max(1), slot_h)
                };

                let thumb_left = cell_left + (cell_w - fit_w) / 2;
                let thumb_top = cell_top + (cell_h - icon_band_h - fit_h) / 2;
                let thumb_rect = RECT {
                    left: thumb_left,
                    top: thumb_top,
                    right: thumb_left + fit_w,
                    bottom: thumb_top + fit_h,
                };

                // Centered on the preview's bottom edge — half over the
                // window, half below it (drawn after the preview, so the
                // overlap actually shows).
                let icon_left = thumb_left + (fit_w - icon_size) / 2;
                let icon_top = thumb_rect.bottom - icon_size / 2;
                let icon_rect = RECT {
                    left: icon_left,
                    top: icon_top,
                    right: icon_left + icon_size,
                    bottom: icon_top + icon_size,
                };

                (thumb_rect, icon_rect, window)
            })
            .collect()
    }

    /// Best-effort small icon for `hwnd` — first the window's own current
    /// icon (`WM_GETICON`, which reflects e.g. a per-document icon some
    /// apps set), falling back to its window class's icon. Returned handles
    /// are owned by the window/class itself and must never be destroyed
    /// here.
    fn window_icon(hwnd: HWND) -> Option<HICON> {
        // SAFETY: `hwnd` is a live top-level window; both queries are
        // synchronous and read-only.
        unsafe {
            let small = SendMessageW(hwnd, WM_GETICON, WPARAM(ICON_SMALL as usize), LPARAM(0));
            if small.0 != 0 {
                return Some(HICON(small.0 as *mut c_void));
            }
            let class_icon = GetClassLongPtrW(hwnd, GCLP_HICONSM);
            if class_icon != 0 {
                return Some(HICON(class_icon as *mut c_void));
            }
            None
        }
    }

    thread_local! {
        /// Cached wallpaper bitmap handle: `Some(0)` means "looked it up
        /// already and there isn't one" (or GDI+ failed), distinct from
        /// not-yet-looked-up (`None`).
        static WALLPAPER_BITMAP: RefCell<Option<isize>> = const { RefCell::new(None) };
    }

    /// Lazily loads and caches the real desktop wallpaper via GDI+ (loaded
    /// once per process — this module doesn't handle a live wallpaper
    /// change). `None` if the wallpaper path lookup or GDI+ decode fails,
    /// in which case callers fall back to a flat color.
    fn wallpaper_bitmap() -> Option<*mut GpBitmap> {
        if let Some(cached) = WALLPAPER_BITMAP.with(|c| *c.borrow()) {
            return (cached != 0).then_some(cached as *mut GpBitmap);
        }
        let bitmap = load_wallpaper_bitmap();
        WALLPAPER_BITMAP.with(|c| *c.borrow_mut() = Some(bitmap.map(|b| b as isize).unwrap_or(0)));
        bitmap
    }

    fn load_wallpaper_bitmap() -> Option<*mut GpBitmap> {
        let mut path_buf = [0u16; 260];
        // SAFETY: `path_buf` is `MAX_PATH`-sized and outlives this
        // synchronous call.
        unsafe {
            SystemParametersInfoW(
                SPI_GETDESKWALLPAPER,
                path_buf.len() as u32,
                Some(path_buf.as_mut_ptr() as *mut c_void),
                Default::default(),
            )
            .ok()?;
        }
        let len = path_buf.iter().position(|&c| c == 0).unwrap_or(0);
        if len == 0 {
            return None;
        }

        let mut bitmap: *mut GpBitmap = std::ptr::null_mut();
        // SAFETY: `path_buf` is nul-terminated (already true within the
        // first `len` characters via the position above; the buffer itself
        // is nul-terminated at worst at its own end) for the duration of
        // this call; `bitmap` is written by GDI+ only on success.
        let status = unsafe { GdipCreateBitmapFromFile(PCWSTR(path_buf.as_ptr()), &mut bitmap) };
        (status.0 == 0 && !bitmap.is_null()).then_some(bitmap)
    }

    thread_local! {
        /// The wallpaper pre-scaled to card size as a plain GDI `HBITMAP`
        /// (`(width, height, handle)`). GDI+'s interpolated rescale of a
        /// multi-megapixel source is far too slow to run once per card per
        /// paint — it made the overview visibly draw in vertical strips and
        /// drop to a few fps while dragging — so it runs once per card
        /// size here and every paint after that is a cheap `BitBlt`.
        /// Every card shares one size (see `card_layout`), so a single
        /// entry suffices; it's re-rendered only if the size changes.
        static WALLPAPER_SCALED: RefCell<Option<(i32, i32, isize)>> = const { RefCell::new(None) };
    }

    fn scaled_wallpaper(width: i32, height: i32) -> Option<isize> {
        if let Some((w, h, handle)) = WALLPAPER_SCALED.with(|c| *c.borrow()) {
            if w == width && h == height {
                return Some(handle);
            }
            // SAFETY: `handle` is an HBITMAP created below on this same
            // thread and owned exclusively by this cache.
            unsafe {
                let _ = DeleteObject(windows::Win32::Graphics::Gdi::HBITMAP(handle as *mut c_void));
            }
            WALLPAPER_SCALED.with(|c| *c.borrow_mut() = None);
        }

        let source = wallpaper_bitmap()?;
        // SAFETY: standard create-select-draw-restore GDI sequence on
        // handles created and torn down entirely within this call (except
        // `bitmap`, whose ownership moves into the cache); `source` was
        // loaded once at startup and never freed/moved.
        unsafe {
            let screen = GetDC(None);
            let mem = CreateCompatibleDC(screen);
            let bitmap = CreateCompatibleBitmap(screen, width, height);
            let previous = SelectObject(mem, bitmap);

            let mut graphics: *mut GpGraphics = std::ptr::null_mut();
            let ok = GdipCreateFromHDC(mem, &mut graphics).0 == 0 && !graphics.is_null();
            if ok {
                let _ = GdipDrawImageRectI(graphics, source as *mut GpImage, 0, 0, width, height);
                let _ = GdipDeleteGraphics(graphics);
            }

            SelectObject(mem, previous);
            let _ = DeleteDC(mem);
            ReleaseDC(None, screen);

            if !ok {
                let _ = DeleteObject(bitmap);
                return None;
            }
            let handle = bitmap.0 as isize;
            WALLPAPER_SCALED.with(|c| *c.borrow_mut() = Some((width, height, handle)));
            Some(handle)
        }
    }

    /// Draws the wallpaper to fill `rect` exactly. The expensive GDI+
    /// rescale runs only when the *base* card size changes (see
    /// `scaled_wallpaper` — one cache entry, stable across frames); an
    /// animated rect (zoom/focus scaling changes card sizes every frame)
    /// is served by a cheap `StretchBlt` from that cached base bitmap.
    /// Keying the cache on the animated size instead was a
    /// full-wallpaper-rescale-per-card-per-frame — single-digit fps.
    /// No-op (leaves whatever fallback fill was already painted there) if
    /// the wallpaper couldn't be loaded.
    fn draw_wallpaper_into(hdc: HDC, rect: RECT) {
        let (base_card, _) = card_layout();
        let (base_w, base_h) = (base_card.right - base_card.left, base_card.bottom - base_card.top);
        let Some(handle) = scaled_wallpaper(base_w, base_h) else {
            return;
        };
        let width = rect.right - rect.left;
        let height = rect.bottom - rect.top;
        // SAFETY: `hdc` is a valid device context for the duration of this
        // call; `handle` is the cache-owned HBITMAP, alive until the next
        // size change.
        unsafe {
            let mem = CreateCompatibleDC(hdc);
            let previous =
                SelectObject(mem, windows::Win32::Graphics::Gdi::HBITMAP(handle as *mut c_void));
            if width == base_w && height == base_h {
                let _ = BitBlt(hdc, rect.left, rect.top, width, height, mem, 0, 0, SRCCOPY);
            } else {
                let _ = StretchBlt(
                    hdc, rect.left, rect.top, width, height, mem, 0, 0, base_w, base_h, SRCCOPY,
                );
            }
            SelectObject(mem, previous);
            let _ = DeleteDC(mem);
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
    fn hide_calendar(restore_focus: bool) {
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

    /// Mirrors [`hide_calendar`] for the Quick Settings flyout.
    fn hide_quick_settings(restore_focus: bool) {
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

    fn toggle_calendar() {
        let info = STATE.with(|s| {
            s.borrow()
                .as_ref()
                .map(|st| (st.calendar_hwnd, st.calendar_open))
        });
        let Some((hwnd, is_open)) = info else {
            return;
        };

        if is_open {
            hide_calendar(true);
            return;
        }

        hide_quick_settings(false);
        close_overview(None);

        // SAFETY: no preconditions.
        let previous_foreground = unsafe { GetForegroundWindow() };
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

    fn toggle_quick_settings() {
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

    /// Re-syncs the workspace tracker against reality — assigns any
    /// currently-visible-but-untracked window and drops assignments for
    /// windows that no longer exist — since this process has no live
    /// window-create/destroy event tracking yet (Phase 1's
    /// `SetWinEventHook` follow-up); every workspace-changing action
    /// re-syncs first instead.
    ///
    /// Assignment rule: a new window belongs to the workspace the user is
    /// actually looking at. When the current workspace is a dynamic one,
    /// that's simply "current" — matching it against monitor position
    /// instead would pull it onto the monitor's pinned workspace, which is
    /// exactly the bug where a window opened on workspace 2 "jumped" back
    /// to workspace 0 at the next sync. Only when the current workspace is
    /// pinned (so every visible workspace is a real monitor) does screen
    /// position decide *which* monitor's workspace it belongs to.
    /// Returns the live snapshot taken to do this, for callers that also
    /// need it (building overview pages).
    fn sync_workspaces() -> Vec<groveshell_window_model::WindowRecord> {
        let live = groveshell_window_model::snapshot();
        let monitors = monitors_sorted_by_x();
        STATE.with(|s| {
            if let Some(state) = s.borrow_mut().as_mut() {
                let current = state.workspaces.current_index();
                let current_pinned = state.workspaces.is_pinned(current);
                for window in &live {
                    if state.workspaces.workspace_of(window.hwnd).is_some() {
                        continue;
                    }
                    let index = if current_pinned {
                        let center_x = (window.rect.left + window.rect.right) / 2;
                        let center_y = (window.rect.top + window.rect.bottom) / 2;
                        monitor_index_for_center(&monitors, center_x, center_y).unwrap_or(current)
                    } else {
                        current
                    };
                    state.workspaces.assign_to_index(window.hwnd, index);
                }
                state.workspaces.prune(groveshell_window_model::is_alive);
                let tracked: Vec<isize> = state
                    .workspaces
                    .workspace_ids()
                    .to_vec()
                    .into_iter()
                    .flat_map(|id| state.workspaces.windows_on(id))
                    .collect();
                retain_window_snapshots(&tracked);
            }
        });
        live
    }

    /// Re-syncs the workspace tracker (see `sync_workspaces`) and builds a
    /// fresh `(cards, thumbs)` pair covering *every* current workspace's
    /// page — one wallpaper-filled card each (see `card_layout`), each
    /// with the workspace's windows gridded inside it (see `layout_grid`).
    /// Every preview is painted from a `WINDOW_SNAPSHOTS` capture: parked
    /// windows already have their park-time capture, and anything without
    /// one (the current workspace's on-screen windows, typically) is
    /// captured here, while its pixels are still available. Used both to
    /// open the overview and to refresh it in place after a workspace
    /// switch (`commit_workspace_switch` calls this rather than patching
    /// the previous session's pages piecemeal).
    fn build_carousel_pages() -> (Vec<CardAnim>, Vec<ThumbAnim>, usize) {
        let live = sync_workspaces();

        let (workspace_ids, current_pos): (Vec<WorkspaceId>, usize) = STATE.with(|s| {
            s.borrow()
                .as_ref()
                .map(|st| (st.workspaces.workspace_ids().to_vec(), st.workspaces.current_index()))
                .unwrap_or_default()
        });

        let (card_rect, _) = card_layout();
        let mut cards = Vec::new();
        let mut thumbs = Vec::new();

        for (page, &ws_id) in workspace_ids.iter().enumerate() {
            cards.push(CardAnim { page, rect: card_rect });

            // A page's windows come from its workspace *assignments*, not
            // from what's on screen — parked windows show up in the live
            // snapshot too, so filtering by assignment is what keeps each
            // window on its own page. The snapshot is just a lookup cache
            // here; anything not in it (e.g. a minimized-and-hidden
            // window) falls back to a direct re-inspection.
            let assigned = STATE
                .with(|s| {
                    s.borrow()
                        .as_ref()
                        .map(|st| st.workspaces.windows_on(ws_id))
                        .unwrap_or_default()
                });
            let windows: Vec<groveshell_window_model::WindowRecord> = assigned
                .into_iter()
                .filter_map(|hwnd| {
                    live.iter()
                        .find(|w| w.hwnd == hwnd)
                        .cloned()
                        .or_else(|| groveshell_window_model::describe(hwnd))
                })
                .collect();

            for (slot_rect, icon_rect, window) in layout_grid(card_rect, windows) {
                let source = HWND(window.hwnd as *mut c_void);
                let icon = window_icon(source);

                // Parked windows were captured at park time; on-screen
                // ones (the current workspace, or another monitor's pinned
                // page) are captured now, while their pixels are still
                // renderable. Failure just means this slot paints as a
                // title chip.
                if window_snapshot(window.hwnd).is_none() {
                    capture_window_snapshot(source);
                }

                thumbs.push(ThumbAnim {
                    hwnd: source,
                    title: window.title,
                    icon,
                    page,
                    rect: slot_rect,
                    icon_rect,
                });
            }
        }

        (cards, thumbs, current_pos)
    }

    /// Shows the overview and fades it in (see the module docs on why
    /// there's no per-window position animation anymore): builds every
    /// workspace's page (`build_carousel_pages`), keeps the bars visually
    /// on top of it (`HWND_TOPMOST` reassertion — the overview is topmost
    /// too, and would otherwise win the z-order fight since it's shown
    /// after them), and kicks off the fade timer. No-op if the overview
    /// isn't currently `Closed` (already open or mid-animation).
    fn open_overview() {
        let overview_hwnd = STATE.with(|s| {
            s.borrow().as_ref().and_then(|st| {
                matches!(st.overview, OverviewMode::Closed).then_some(st.overview_hwnd)
            })
        });
        let Some(overview_hwnd) = overview_hwnd else {
            return;
        };

        // The overview covers the same area any open flyout would; keeping
        // one of those open underneath it would just be confusing dead
        // state.
        hide_calendar(false);
        hide_quick_settings(false);

        let (cards, thumbs, current_pos) = build_carousel_pages();

        // SAFETY: no preconditions.
        let previous_foreground = unsafe { GetForegroundWindow() };

        STATE.with(|s| {
            if let Some(state) = s.borrow_mut().as_mut() {
                state.previous_foreground = previous_foreground;
                state.carousel_offset = current_pos as f64;
                state.carousel_drag = None;
                state.carousel_anim = None;
                state.carousel_close_after = None;
                state.overview = OverviewMode::Opening {
                    started: Instant::now(),
                    thumbs,
                    cards,
                };
            }
        });

        // SAFETY: `overview_hwnd` is a valid, process-lifetime window.
        unsafe {
            let _ = SetLayeredWindowAttributes(overview_hwnd, COLORREF(0), 0, LWA_ALPHA);
            let _ = ShowWindow(overview_hwnd, SW_SHOW);
            let _ = SetForegroundWindow(overview_hwnd);
            let _ = SetFocus(overview_hwnd);
            raise_bars_topmost();
            SetTimer(overview_hwnd, ANIM_TIMER_ID, ANIM_TIMER_INTERVAL_MS, None);
        }
    }

    /// Puts every bar back above the overview within the topmost band.
    /// Needed once at open (the overview is shown and activated *after*
    /// the bars) and again every time the overview is clicked — mouse
    /// activation re-raises the overview above its topmost siblings, which
    /// is exactly how the bar "stopped rendering" the moment a drag
    /// started.
    fn raise_bars_topmost() {
        let bar_hwnds: Vec<HWND> = STATE.with(|s| {
            s.borrow()
                .as_ref()
                .map(|st| st.bars.iter().map(|b| b.hwnd).collect())
                .unwrap_or_default()
        });
        // SAFETY: every bar hwnd is a valid, process-lifetime window.
        unsafe {
            for bar_hwnd in bar_hwnds {
                let _ = SetWindowPos(
                    bar_hwnd,
                    HWND_TOPMOST,
                    0,
                    0,
                    0,
                    0,
                    SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                );
            }
        }
    }

    /// Refreshes every page's cards/thumbnails in place (see
    /// `build_carousel_pages`) without touching `carousel_offset` or
    /// replaying the open animation — used after `commit_workspace_switch`
    /// while the overview is already `Open`, since that can change which
    /// workspace is current, how many workspaces exist (the dynamic-
    /// workspace policy grows/shrinks the trailing empty one), and which
    /// windows live where, all at once. Re-registers live thumbnails from
    /// scratch rather than patching three kinds of drift piecemeal.
    /// No-op if the overview isn't `Open`.
    fn rebuild_open_overview_pages() {
        let overview_hwnd = STATE.with(|s| {
            s.borrow().as_ref().and_then(|st| {
                matches!(st.overview, OverviewMode::Open { .. }).then_some(st.overview_hwnd)
            })
        });
        let Some(overview_hwnd) = overview_hwnd else {
            return;
        };

        let (cards, thumbs, _current_pos) = build_carousel_pages();

        STATE.with(|s| {
            if let Some(state) = s.borrow_mut().as_mut() {
                if matches!(state.overview, OverviewMode::Open { .. }) {
                    state.overview = OverviewMode::Open { thumbs, cards };
                }
            }
        });

        repaint_overview(overview_hwnd);
    }

    /// Starts the fade-out, then hides the overview and focuses
    /// `focus_after` (or restores whatever was focused before Activities
    /// was opened, if `None`) once it finishes. Works whether the overview
    /// is currently `Open` (idle) or still `Opening` (interrupts it
    /// smoothly from its current alpha — no-op only if already `Closed` or
    /// already `Closing`).
    fn close_overview(focus_after: Option<HWND>) {
        let overview_hwnd = STATE.with(|s| {
            let mut state_ref = s.borrow_mut();
            let Some(state) = state_ref.as_mut() else {
                return None;
            };

            let mode = std::mem::replace(&mut state.overview, OverviewMode::Closed);
            match mode {
                OverviewMode::Open { thumbs, cards } | OverviewMode::Opening { thumbs, cards, .. } => {
                    state.overview = OverviewMode::Closing {
                        started: Instant::now(),
                        thumbs,
                        cards,
                        focus_after,
                    };
                    // Any in-progress carousel drag/slide is moot once
                    // we're fading back out.
                    state.carousel_drag = None;
                    state.carousel_anim = None;
                    state.carousel_close_after = None;
                    Some(state.overview_hwnd)
                }
                other => {
                    state.overview = other;
                    None
                }
            }
        });

        let Some(overview_hwnd) = overview_hwnd else {
            return;
        };

        // SAFETY: `overview_hwnd` is a valid, process-lifetime window.
        unsafe {
            SetTimer(overview_hwnd, ANIM_TIMER_ID, ANIM_TIMER_INTERVAL_MS, None);
        }
    }

    /// Dispatches a click on the primary bar to whichever of its three
    /// painted regions it landed in (see `paint_bar` for the same layout,
    /// including the DPI scaling both must agree on).
    fn on_bar_click(hwnd: HWND, x: i32) {
        // SAFETY: `hwnd` is the bar window currently handling this click.
        let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
        let bar_width = STATE.with(|s| {
            s.borrow()
                .as_ref()
                .map(|st| st.primary_bar_rect.right - st.primary_bar_rect.left)
        });
        let Some(bar_width) = bar_width else {
            return;
        };

        if (scaled(ACTIVITIES_LABEL_X, dpi)..scaled(ACTIVITIES_LABEL_X + ACTIVITIES_LABEL_WIDTH, dpi))
            .contains(&x)
        {
            let is_closed = STATE.with(|s| {
                s.borrow()
                    .as_ref()
                    .map(|st| matches!(st.overview, OverviewMode::Closed))
            });
            match is_closed {
                Some(true) => open_overview(),
                Some(false) => close_overview(None),
                None => {}
            }
            return;
        }

        let workspace_count = STATE
            .with(|s| s.borrow().as_ref().map(|st| st.workspaces.workspace_ids().len()))
            .unwrap_or(0);
        let dots_x = scaled(WS_DOTS_X, dpi);
        let dot_slot_w = scaled(WS_DOT_SLOT_WIDTH, dpi);
        let dots_width = workspace_count as i32 * dot_slot_w;
        if (dots_x..dots_x + dots_width).contains(&x) {
            let index = ((x - dots_x) / dot_slot_w) as usize;
            let overview_open = STATE
                .with(|s| s.borrow().as_ref().map(|st| matches!(st.overview, OverviewMode::Open { .. })))
                .unwrap_or(false);
            if overview_open {
                snap_carousel_to(index, None);
            } else {
                commit_workspace_switch(index);
            }
            return;
        }

        let clock_w = scaled(CLOCK_LABEL_WIDTH, dpi);
        let clock_x = bar_width / 2 - clock_w / 2;
        if (clock_x..clock_x + clock_w).contains(&x) {
            toggle_calendar();
            return;
        }

        let qs_x = bar_width - scaled(QS_LABEL_WIDTH + QS_LABEL_MARGIN, dpi);
        if (qs_x..bar_width - scaled(QS_LABEL_MARGIN, dpi)).contains(&x) {
            toggle_quick_settings();
        }
    }

    enum OverviewHit {
        Window { page: usize, hwnd: HWND },
        EmptyPage { page: usize },
    }

    /// Hit-tests a click against the overview's current thumbnail/card
    /// rects across *every* carousel page (only meaningful while `Open`),
    /// applying each page's current carousel shift first. Clicking a
    /// window focuses it, switching to its workspace first if it isn't
    /// already current. Clicking empty space on a *different* page just
    /// re-centers the carousel there (like clicking a workspace thumbnail
    /// in GNOME) without closing the overview. Clicking empty space on the
    /// current page, or missing everything, cancels — the pre-existing
    /// behavior.
    fn on_overview_click(x: i32, y: i32) {
        let (hit, current) = STATE.with(|s| {
            let state = s.borrow();
            let Some(st) = state.as_ref() else {
                return (None, 0);
            };
            let current = st.workspaces.current_index();
            let OverviewMode::Open { thumbs, cards } = &st.overview else {
                return (None, current);
            };
            let (card_rect, pitch) = card_layout();
            let thumb_hit = thumbs.iter().find_map(|th| {
                let r = displayed_rect(th.rect, th.page, st.carousel_offset, pitch, card_rect);
                (x >= r.left && x < r.right && y >= r.top && y < r.bottom)
                    .then_some(OverviewHit::Window { page: th.page, hwnd: th.hwnd })
            });
            let hit = thumb_hit.or_else(|| {
                cards.iter().find_map(|c| {
                    let r = displayed_rect(c.rect, c.page, st.carousel_offset, pitch, card_rect);
                    (x >= r.left && x < r.right && y >= r.top && y < r.bottom)
                        .then_some(OverviewHit::EmptyPage { page: c.page })
                })
            });
            (hit, current)
        });

        match hit {
            Some(OverviewHit::Window { page, hwnd }) if page == current => close_overview(Some(hwnd)),
            Some(OverviewHit::Window { page, hwnd }) => snap_carousel_to(page, Some(hwnd)),
            Some(OverviewHit::EmptyPage { page }) if page != current => snap_carousel_to(page, None),
            _ => close_overview(None),
        }
    }

    /// Starts tracking a possible carousel drag; only takes effect while
    /// the overview is idle-`Open` (matching the existing hit-testing
    /// gate — no dragging mid zoom-animation).
    fn on_overview_drag_start(x: i32) {
        // The click that starts this drag just re-activated (and re-raised)
        // the overview — put the bars back on top of it.
        raise_bars_topmost();
        STATE.with(|s| {
            if let Some(state) = s.borrow_mut().as_mut() {
                if matches!(state.overview, OverviewMode::Open { .. }) {
                    state.carousel_drag = Some(CarouselDrag {
                        start_x: x,
                        start_offset: state.carousel_offset,
                        max_delta: 0,
                    });
                }
            }
        });
    }

    /// Follows the pointer while a carousel drag is active — content moves
    /// with the cursor (dragging right reveals the *previous*, lower-index
    /// workspace), at a fixed `CAROUSEL_DRAG_PAGE_DISTANCE_PX`-per-page
    /// rate rather than the overview's actual (screen-spanning) pixel
    /// width — see that constant's docs.
    fn on_overview_drag_move(x: i32) {
        let overview_hwnd = STATE.with(|s| {
            let mut state_ref = s.borrow_mut();
            let state = state_ref.as_mut()?;
            let workspace_count = state.workspaces.workspace_ids().len();
            let drag = state.carousel_drag.as_mut()?;
            let delta_px = x - drag.start_x;
            drag.max_delta = drag.max_delta.max(delta_px.abs());
            let raw_offset = drag.start_offset - delta_px as f64 / CAROUSEL_DRAG_PAGE_DISTANCE_PX;
            state.carousel_offset = raw_offset.clamp(0.0, (workspace_count.max(1) - 1) as f64);
            Some(state.overview_hwnd)
        });
        if let Some(overview_hwnd) = overview_hwnd {
            repaint_overview(overview_hwnd);
        }
    }

    /// Ends a carousel drag (or, if there wasn't one — the button went down
    /// outside `Open`, or never moved past the click threshold — dispatches
    /// a plain click instead). A real drag snaps to whichever page ended up
    /// nearest the release point.
    fn on_overview_drag_end(x: i32, y: i32) {
        let drag = STATE.with(|s| s.borrow_mut().as_mut().and_then(|st| st.carousel_drag.take()));
        let Some(drag) = drag else {
            on_overview_click(x, y);
            return;
        };

        if drag.max_delta <= CAROUSEL_DRAG_CLICK_THRESHOLD_PX {
            // Not actually a drag — a tiny nudge from drag_move may have
            // moved carousel_offset by a pixel or two; put it back exactly
            // before treating this as an ordinary click.
            STATE.with(|s| {
                if let Some(state) = s.borrow_mut().as_mut() {
                    state.carousel_offset = drag.start_offset;
                }
            });
            on_overview_click(x, y);
            return;
        }

        let target = STATE.with(|s| {
            s.borrow()
                .as_ref()
                .map(|st| {
                    let max_index = st.workspaces.workspace_ids().len().saturating_sub(1);
                    st.carousel_offset.round().clamp(0.0, max_index as f64) as usize
                })
                .unwrap_or(0)
        });
        snap_carousel_to(target, None);
    }

    /// Left/Right arrow keys while the overview is idle-`Open`: slide the
    /// carousel by one workspace, same as the global hotkeys but without
    /// leaving the overview.
    fn on_overview_arrow(delta: i32) {
        let target = STATE.with(|s| {
            s.borrow().as_ref().and_then(|st| {
                matches!(st.overview, OverviewMode::Open { .. })
                    .then(|| st.workspaces.clamped_relative_index(delta))
            })
        });
        if let Some(target) = target {
            snap_carousel_to(target, None);
        }
    }

    /// How far below its real position a window is moved when its
    /// workspace stops being current. Parked off-screen rather than
    /// `SW_HIDE`-hidden deliberately: DWM renders *nothing* for a hidden
    /// window's thumbnail — an off-screen window at least stays a real,
    /// visible window. Far enough down to clear any plausible monitor
    /// arrangement, small enough to stay well inside the ±32767 coordinate
    /// space, and vertical so the window's nearest monitor (and therefore
    /// its DPI) doesn't change.
    ///
    /// Off-screen alone isn't enough for previews, though: many apps
    /// (browsers, UWP) detect they're fully occluded/off-screen and stop
    /// producing frames, and DWM can evict their surface entirely — which
    /// made parked windows' live thumbnails go blank shortly after
    /// parking. That's why `park_window` also captures a static snapshot
    /// first (see `WINDOW_SNAPSHOTS`); the overview paints that instead of
    /// a live thumbnail for parked windows.
    const WORKSPACE_PARK_DY: i32 = 20000;

    thread_local! {
        /// Per-window `PrintWindow` captures taken at park time, keyed by
        /// hwnd: `(width, height, HBITMAP)`. A parked window's snapshot is
        /// exactly its last on-screen appearance, so painting it in the
        /// overview is indistinguishable from a live thumbnail until the
        /// app would have next repainted anyway. Dropped (and the bitmap
        /// destroyed) on unpark, on prune of a dead window, and never
        /// duplicated — a re-capture replaces the old entry.
        static WINDOW_SNAPSHOTS: RefCell<std::collections::BTreeMap<isize, (i32, i32, isize)>> =
            const { RefCell::new(std::collections::BTreeMap::new()) };
    }

    fn window_snapshot(hwnd: isize) -> Option<(i32, i32, isize)> {
        WINDOW_SNAPSHOTS.with(|s| s.borrow().get(&hwnd).copied())
    }

    fn drop_window_snapshot(hwnd: isize) {
        if let Some((_, _, bitmap)) = WINDOW_SNAPSHOTS.with(|s| s.borrow_mut().remove(&hwnd)) {
            // SAFETY: the handle was created by `capture_window_snapshot`
            // on this thread and is owned exclusively by the map.
            unsafe {
                let _ = DeleteObject(HBITMAP(bitmap as *mut c_void));
            }
        }
        if let Some((_, _, bitmap)) = SCALED_SNAPSHOTS.with(|s| s.borrow_mut().remove(&hwnd)) {
            // SAFETY: same ownership story, created by `slot_scaled_snapshot`.
            unsafe {
                let _ = DeleteObject(HBITMAP(bitmap as *mut c_void));
            }
        }
    }

    thread_local! {
        /// Per-window snapshots pre-scaled (once, with HALFTONE quality)
        /// to their overview grid-slot size, keyed by hwnd. The full-size
        /// capture in `WINDOW_SNAPSHOTS` is multi-megapixel; stretching it
        /// per window per frame was a large part of the overview's
        /// animation cost. Per-frame painting stretches from *this* small
        /// bitmap instead (near-1:1 during zoom/focus scaling, so cheap
        /// and visually clean). Re-derived whenever the slot size changes
        /// (layout/grid changes) and dropped alongside the full capture.
        static SCALED_SNAPSHOTS: RefCell<std::collections::BTreeMap<isize, (i32, i32, isize)>> =
            const { RefCell::new(std::collections::BTreeMap::new()) };
    }

    /// The window's snapshot pre-scaled to `(w, h)` — served from cache
    /// when the size matches, rebuilt from the full capture otherwise.
    fn slot_scaled_snapshot(hwnd: isize, w: i32, h: i32) -> Option<isize> {
        if w <= 0 || h <= 0 {
            return None;
        }
        if let Some((cw, ch, handle)) = SCALED_SNAPSHOTS.with(|s| s.borrow().get(&hwnd).copied()) {
            if cw == w && ch == h {
                return Some(handle);
            }
            // SAFETY: cache-owned HBITMAP created below on this thread.
            unsafe {
                let _ = DeleteObject(HBITMAP(handle as *mut c_void));
            }
            SCALED_SNAPSHOTS.with(|s| s.borrow_mut().remove(&hwnd));
        }

        let (src_w, src_h, src_handle) = window_snapshot(hwnd)?;
        // SAFETY: standard create-select-stretch-restore GDI sequence on
        // locally created handles; `bitmap`'s ownership moves into the
        // cache on success.
        unsafe {
            let screen = GetDC(None);
            let src = CreateCompatibleDC(screen);
            let dst = CreateCompatibleDC(screen);
            let bitmap = CreateCompatibleBitmap(screen, w, h);
            let previous_src = SelectObject(src, HBITMAP(src_handle as *mut c_void));
            let previous_dst = SelectObject(dst, bitmap);
            SetStretchBltMode(dst, HALFTONE);
            let ok = StretchBlt(dst, 0, 0, w, h, src, 0, 0, src_w, src_h, SRCCOPY).as_bool();
            SelectObject(src, previous_src);
            SelectObject(dst, previous_dst);
            let _ = DeleteDC(src);
            let _ = DeleteDC(dst);
            ReleaseDC(None, screen);

            if !ok {
                let _ = DeleteObject(bitmap);
                return None;
            }
            let handle = bitmap.0 as isize;
            SCALED_SNAPSHOTS.with(|s| {
                s.borrow_mut().insert(hwnd, (w, h, handle));
            });
            Some(handle)
        }
    }

    /// Drops snapshots for every window not in `tracked` — dead windows
    /// are pruned from the workspace tracker without passing through
    /// `unpark_window`, so their bitmaps would otherwise leak.
    fn retain_window_snapshots(tracked: &[isize]) {
        let stale: Vec<isize> = WINDOW_SNAPSHOTS.with(|s| {
            s.borrow()
                .keys()
                .filter(|hwnd| !tracked.contains(hwnd))
                .copied()
                .collect()
        });
        for hwnd in stale {
            drop_window_snapshot(hwnd);
        }
    }

    /// Captures `hwnd`'s current on-screen appearance into the snapshot
    /// store. Must run *before* the window is parked — `PrintWindow` with
    /// `PW_RENDERFULLCONTENT` (undocumented-but-stable flag 2, the one
    /// that goes through DWM and so handles D3D/DComp-rendered content)
    /// asks the app/DWM for current pixels, which is exactly what stops
    /// being available once the window is off-screen. Best-effort: on any
    /// failure the window simply has no snapshot and its overview slot
    /// falls back to a live-thumbnail attempt or a title chip.
    fn capture_window_snapshot(hwnd: HWND) {
        // SAFETY: create-select-print-restore on locally created GDI
        // handles; `bitmap`'s ownership moves into the store on success.
        unsafe {
            let mut rect = RECT::default();
            if GetWindowRect(hwnd, &mut rect).is_err() {
                return;
            }
            let (w, h) = (rect.right - rect.left, rect.bottom - rect.top);
            if w <= 0 || h <= 0 {
                return;
            }
            let screen = GetDC(None);
            let mem = CreateCompatibleDC(screen);
            let bitmap = CreateCompatibleBitmap(screen, w, h);
            let previous = SelectObject(mem, bitmap);
            let ok = PrintWindow(hwnd, mem, PRINT_WINDOW_FLAGS(2)).as_bool();
            SelectObject(mem, previous);
            let _ = DeleteDC(mem);
            ReleaseDC(None, screen);

            if !ok {
                let _ = DeleteObject(bitmap);
                return;
            }
            drop_window_snapshot(hwnd.0 as isize);
            WINDOW_SNAPSHOTS.with(|s| {
                s.borrow_mut().insert(hwnd.0 as isize, (w, h, bitmap.0 as isize));
            });
        }
    }

    /// Moves `hwnd` to its parked (off-screen) position. Skips minimized
    /// windows — their on-screen rect is a `-32000` placeholder whose
    /// restore position Windows manages separately, and they have no live
    /// pixels to keep renderable anyway. The `top` guard makes parking
    /// idempotent (and keeps a window left parked by a crashed previous
    /// run from being pushed further out).
    fn park_window(hwnd: HWND) {
        // SAFETY: `hwnd` was tracked as a real window; if it has since
        // closed every call here documented-fails harmlessly.
        unsafe {
            if IsIconic(hwnd).as_bool() {
                let _ = ShowWindow(hwnd, SW_HIDE);
                return;
            }
            let mut rect = RECT::default();
            if GetWindowRect(hwnd, &mut rect).is_err() || rect.top >= WORKSPACE_PARK_DY / 2 {
                return;
            }
            capture_window_snapshot(hwnd);
            let _ = SetWindowPos(
                hwnd,
                HWND(std::ptr::null_mut()),
                rect.left,
                rect.top + WORKSPACE_PARK_DY,
                0,
                0,
                SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
    }

    /// Inverse of [`park_window`]: returns `hwnd` to its real position (a
    /// fixed offset, so no per-window bookkeeping to drift out of date)
    /// and retires its park-time snapshot — the live window is preview
    /// material again.
    fn unpark_window(hwnd: HWND) {
        drop_window_snapshot(hwnd.0 as isize);
        // SAFETY: same as `park_window`.
        unsafe {
            if IsIconic(hwnd).as_bool() {
                let _ = ShowWindow(hwnd, SW_SHOWNA);
                return;
            }
            let mut rect = RECT::default();
            if GetWindowRect(hwnd, &mut rect).is_err() || rect.top < WORKSPACE_PARK_DY / 2 {
                // Not parked — e.g. it was minimized (and hidden) at park
                // time; make sure it's shown either way.
                let _ = ShowWindow(hwnd, SW_SHOWNA);
                return;
            }
            let _ = SetWindowPos(
                hwnd,
                HWND(std::ptr::null_mut()),
                rect.left,
                rect.top - WORKSPACE_PARK_DY,
                0,
                0,
                SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
    }

    /// Commits to the workspace at `target_index`: parks the outgoing
    /// workspace's windows off-screen and unparks the incoming ones (see
    /// `park_window` for why this isn't a `ShowWindow` hide/show swap), so
    /// the desktop actually reflects the new current workspace
    /// immediately, even if the overview stays open — GNOME switches live
    /// as soon as you land on a workspace in the overview too, rather than
    /// waiting for it to close. No-op if `target_index` is already
    /// current.
    fn commit_workspace_switch(target_index: usize) {
        let switch = STATE.with(|s| {
            let mut state_ref = s.borrow_mut();
            let state = state_ref.as_mut()?;
            let from_pinned = state.workspaces.is_pinned(state.workspaces.current_index());
            let to_pinned = state.workspaces.is_pinned(target_index);
            let (from_id, to_id) = state.workspaces.switch_to_index(target_index)?;
            // Two pinned workspaces are two physical monitors, both already
            // on screen — switching between them is purely a focus/indicator
            // change, nothing to hide or show. Any other pair shares screen
            // space (a dynamic workspace displays over the monitor being
            // left), so the outgoing windows must actually hide and the
            // incoming ones actually show — with a single monitor, its one
            // pinned workspace included, or switching does nothing visible.
            let both_pinned = from_pinned && to_pinned;
            let hide = if both_pinned { Vec::new() } else { state.workspaces.windows_on(from_id) };
            let show = if both_pinned { Vec::new() } else { state.workspaces.windows_on(to_id) };
            Some((hide, show))
        });
        let Some((hide, show)) = switch else {
            return;
        };

        for hwnd in &hide {
            park_window(HWND(*hwnd as *mut c_void));
        }
        for hwnd in &show {
            unpark_window(HWND(*hwnd as *mut c_void));
        }

        // Rebuild the overview's pages from scratch if it's open: a
        // workspace switch can change which page is current, how many
        // workspaces exist at all (dynamic growth/shrink), and which
        // windows live where, so patching the previous session's pages
        // piecemeal was a correctness trap — see `rebuild_open_overview_pages`.
        rebuild_open_overview_pages();
        refresh_bar_indicator();
    }

    /// Commits to `target_index` immediately and starts a smooth visual
    /// slide of the carousel to center on it (only if the overview is
    /// idle-`Open`; otherwise the switch still happens, just with nothing
    /// to animate). If `close_after` is `Some`, the overview closes
    /// (focusing that window) once the slide lands.
    fn snap_carousel_to(target_index: usize, close_after: Option<HWND>) {
        commit_workspace_switch(target_index);

        let overview_hwnd = STATE.with(|s| {
            let mut state_ref = s.borrow_mut();
            let state = state_ref.as_mut()?;
            if !matches!(state.overview, OverviewMode::Open { .. }) {
                return None;
            }
            let from = state.carousel_offset;
            let to = target_index as f64;
            state.carousel_drag = None;
            if (from - to).abs() < 0.001 && close_after.is_none() {
                state.carousel_anim = None;
                return None;
            }
            state.carousel_anim = Some(CarouselAnim { started: Instant::now(), from, to });
            state.carousel_close_after = close_after;
            Some(state.overview_hwnd)
        });

        if let Some(overview_hwnd) = overview_hwnd {
            // SAFETY: `overview_hwnd` is a valid, process-lifetime window.
            unsafe {
                SetTimer(overview_hwnd, ANIM_TIMER_ID, ANIM_TIMER_INTERVAL_MS, None);
            }
        }
    }

    /// Queues a repaint of the overview — everything it shows (cards,
    /// snapshots, chips, icons) is read fresh from state and transformed
    /// at paint time, so an invalidate is all a position/zoom change ever
    /// needs. `erase = false`: this fires on every drag `WM_MOUSEMOVE`,
    /// and `paint_overview` fully repaints (double-buffered) each time —
    /// erasing first just added a background flash, visible as flicker.
    fn repaint_overview(overview_hwnd: HWND) {
        // SAFETY: `overview_hwnd` is a valid, process-lifetime window.
        unsafe {
            let _ = InvalidateRect(overview_hwnd, None, false);
        }
    }

    fn refresh_bar_indicator() {
        let primary = STATE.with(|s| s.borrow().as_ref().map(|st| st.primary_bar_hwnd));
        if let Some(primary) = primary {
            // SAFETY: `primary` is a valid, process-lifetime window.
            unsafe {
                let _ = InvalidateRect(primary, None, true);
            }
        }
    }

    /// `Ctrl+Alt+Left/Right` — works whether or not the overview is open;
    /// re-syncs the workspace tracker first (see `sync_workspaces`) so any
    /// window opened since the last sync lands on the workspace being left
    /// rather than silently following onto the new one.
    fn switch_workspace_relative(delta: i32) {
        sync_workspaces();
        let (target, overview_open) = STATE.with(|s| {
            s.borrow()
                .as_ref()
                .map(|st| {
                    (
                        st.workspaces.clamped_relative_index(delta),
                        matches!(st.overview, OverviewMode::Open { .. }),
                    )
                })
                .unwrap_or((0, false))
        });
        if overview_open {
            snap_carousel_to(target, None);
        } else {
            commit_workspace_switch(target);
        }
    }

    /// `Ctrl+Alt+Shift+Left/Right` — sends the foreground window away into
    /// the dynamic (non-monitor) tail and parks it immediately (the user
    /// stays on their current monitor; only the window leaves). `delta`'s
    /// *sign* doesn't actually matter: a focused window can only ever be on
    /// a pinned monitor workspace (it has to be visible to be focused, and
    /// only monitor workspaces are ever really visible), and there's no
    /// meaningful "direction" for leaving a monitor — it always lands on
    /// the first dynamic workspace either way. No-op if nothing eligible is
    /// focused (including any of this shell's own windows).
    fn move_focused_window_relative(delta: i32) {
        let _ = delta;
        // SAFETY: no preconditions.
        let fg = unsafe { GetForegroundWindow() };
        if fg.0.is_null() || role_of(fg) != Role::Other {
            return;
        }
        sync_workspaces();
        let hwnd = fg.0 as isize;
        let moved = STATE.with(|s| {
            let mut state_ref = s.borrow_mut();
            let state = state_ref.as_mut()?;
            let target = state.workspaces.pinned_count();
            state.workspaces.move_window_to_index(hwnd, target)
        });
        if moved.is_some() {
            park_window(fg);
        }
        refresh_bar_indicator();
    }

    /// Advances whichever animation is in flight by one tick: the
    /// `Opening`/`Closing` whole-window fade, and/or an independent
    /// in-flight carousel slide (these can run at the same time, or a
    /// carousel slide can run on its own while the overview just sits
    /// `Open`) — or finalizes either once it reaches the end.
    fn on_animation_tick() {
        enum Completion {
            Opened,
            Closed { focus_after: Option<HWND> },
        }

        let result = STATE.with(|s| {
            let mut state_ref = s.borrow_mut();
            let state = state_ref.as_mut()?;

            let mut carousel_done = false;
            if let Some(anim) = &state.carousel_anim {
                let t = progress_dur(anim.started, CAROUSEL_SNAP_DURATION);
                state.carousel_offset = anim.from + (anim.to - anim.from) * ease_out(t);
                if t >= 1.0 {
                    state.carousel_offset = anim.to;
                    state.carousel_anim = None;
                    carousel_done = true;
                }
            }

            let mode = std::mem::replace(&mut state.overview, OverviewMode::Closed);
            let (new_mode, fade_alpha, fade_running, completion) = match mode {
                OverviewMode::Opening { started, thumbs, cards } => {
                    let t = progress(started);
                    let alpha = (ease_out(t) * 255.0).round() as u8;
                    if t >= 1.0 {
                        (OverviewMode::Open { thumbs, cards }, Some(255u8), false, Some(Completion::Opened))
                    } else {
                        (OverviewMode::Opening { started, thumbs, cards }, Some(alpha), true, None)
                    }
                }
                OverviewMode::Closing { started, thumbs, cards, focus_after } => {
                    let t = progress(started);
                    let alpha = ((1.0 - ease_out(t)) * 255.0).round() as u8;
                    if t >= 1.0 {
                        let _ = thumbs;
                        (OverviewMode::Closed, None, false, Some(Completion::Closed { focus_after }))
                    } else {
                        (
                            OverviewMode::Closing { started, thumbs, cards, focus_after },
                            Some(alpha),
                            true,
                            None,
                        )
                    }
                }
                other => (other, None, false, None),
            };
            state.overview = new_mode;

            let carousel_close_after = if carousel_done { state.carousel_close_after.take() } else { None };
            let keep_timer = fade_running || state.carousel_anim.is_some();
            Some((state.overview_hwnd, fade_alpha, completion, carousel_close_after, keep_timer))
        });

        let Some((overview_hwnd, fade_alpha, completion, carousel_close_after, keep_timer)) = result else {
            return;
        };

        if let Some(alpha) = fade_alpha {
            // SAFETY: `overview_hwnd` is a valid, process-lifetime window
            // already created with `WS_EX_LAYERED`.
            unsafe {
                let _ = SetLayeredWindowAttributes(overview_hwnd, COLORREF(0), alpha, LWA_ALPHA);
            }
        }

        // Repaint with whatever `carousel_offset`/zoom this tick produced,
        // whether or not the fade alpha changed.
        repaint_overview(overview_hwnd);

        if !keep_timer {
            // SAFETY: `overview_hwnd` is a valid, process-lifetime window.
            unsafe {
                let _ = KillTimer(overview_hwnd, ANIM_TIMER_ID);
            }
        }

        if let Some(completion) = completion {
            match completion {
                Completion::Opened => {}
                Completion::Closed { focus_after } => {
                    // SAFETY: `overview_hwnd` is a valid, process-lifetime
                    // window.
                    unsafe {
                        let _ = ShowWindow(overview_hwnd, SW_HIDE);
                    }

                    let target = focus_after.or_else(|| {
                        STATE
                            .with(|s| s.borrow().as_ref().map(|st| st.previous_foreground))
                            .filter(|h| !h.0.is_null() && *h != overview_hwnd)
                    });
                    if let Some(target) = target {
                        // SAFETY: `target` was either just clicked (still
                        // alive) or captured moments ago by `GetForegroundWindow`;
                        // if it has since been destroyed these calls are
                        // documented no-ops/failures, not undefined behavior.
                        unsafe {
                            if IsIconic(target).as_bool() {
                                let _ = ShowWindow(target, SW_RESTORE);
                            }
                            let _ = SetForegroundWindow(target);
                        }
                    }
                }
            }
        }

        // A carousel slide that just landed on a page reached by clicking
        // one of its thumbnails (`snap_carousel_to(_, Some(hwnd))`) closes
        // the overview onto that window, same as clicking a current-page
        // thumbnail does directly.
        if let Some(hwnd) = carousel_close_after {
            close_overview(Some(hwnd));
        }
    }

    fn progress_dur(started: Instant, duration: Duration) -> f64 {
        (started.elapsed().as_secs_f64() / duration.as_secs_f64()).min(1.0)
    }

    fn progress(started: Instant) -> f64 {
        progress_dur(started, ANIM_DURATION)
    }

    unsafe extern "system" fn wndproc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        let role = role_of(hwnd);

        match msg {
            WM_PAINT => match role {
                Role::Bar { is_primary } => {
                    paint_bar(hwnd, is_primary);
                    LRESULT(0)
                }
                Role::Overview => {
                    paint_overview(hwnd);
                    LRESULT(0)
                }
                Role::Calendar => {
                    paint_calendar(hwnd);
                    LRESULT(0)
                }
                Role::QuickSettings => {
                    paint_quick_settings(hwnd);
                    LRESULT(0)
                }
                Role::Other => DefWindowProcW(hwnd, msg, wparam, lparam),
            },
            // The overview paints its own backdrop into a back buffer (see
            // `paint_overview`), so the class-brush erase pass is both
            // redundant and the source of a visible background flash
            // between erase and repaint while dragging — claim it handled.
            WM_ERASEBKGND if role == Role::Overview => LRESULT(1),
            WM_LBUTTONDOWN => {
                if let Role::Overview = role {
                    let x = (lparam.0 & 0xFFFF) as i32;
                    on_overview_drag_start(x);
                    LRESULT(0)
                } else {
                    DefWindowProcW(hwnd, msg, wparam, lparam)
                }
            }
            WM_MOUSEMOVE => {
                if let Role::Overview = role {
                    if wparam.0 & (MK_LBUTTON.0 as usize) != 0 {
                        let x = (lparam.0 & 0xFFFF) as i32;
                        on_overview_drag_move(x);
                    }
                }
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
            WM_LBUTTONUP => {
                match role {
                    Role::Bar { is_primary: true } => {
                        let x = (lparam.0 & 0xFFFF) as i32;
                        on_bar_click(hwnd, x);
                    }
                    Role::Overview => {
                        let x = (lparam.0 & 0xFFFF) as i32;
                        let y = ((lparam.0 >> 16) & 0xFFFF) as i32;
                        on_overview_drag_end(x, y);
                    }
                    _ => {}
                }
                LRESULT(0)
            }
            WM_KEYDOWN => {
                if wparam.0 == VK_ESCAPE.0 as usize {
                    match role {
                        Role::Overview => close_overview(None),
                        Role::Calendar => hide_calendar(true),
                        Role::QuickSettings => hide_quick_settings(true),
                        _ => {}
                    }
                } else if role == Role::Overview {
                    if wparam.0 == VK_LEFT.0 as usize {
                        on_overview_arrow(-1);
                    } else if wparam.0 == VK_RIGHT.0 as usize {
                        on_overview_arrow(1);
                    }
                }
                LRESULT(0)
            }
            WM_ACTIVATE => {
                // LOWORD(wParam) == WA_INACTIVE means this window just lost
                // activation — e.g. the user clicked somewhere else — which
                // is this shell's cue to auto-dismiss a flyout, the same as
                // clicking away from the real taskbar's date/time or quick
                // settings panel does.
                if (wparam.0 & 0xFFFF) as u32 == WA_INACTIVE {
                    match role {
                        Role::Calendar => hide_calendar(false),
                        Role::QuickSettings => hide_quick_settings(false),
                        _ => {}
                    }
                }
                DefWindowProcW(hwnd, msg, wparam, lparam)
            }
            WM_COMMAND if role == Role::QuickSettings => {
                let control_id = (wparam.0 & 0xFFFF) as i32;
                let notify_code = ((wparam.0 >> 16) & 0xFFFF) as u32;
                if notify_code == BN_CLICKED {
                    match control_id {
                        QS_VOL_DOWN => adjust_volume(-5),
                        QS_VOL_UP => adjust_volume(5),
                        QS_MUTE => toggle_mute(),
                        _ => {}
                    }
                    let _ = InvalidateRect(hwnd, None, true);
                }
                LRESULT(0)
            }
            WM_HOTKEY => {
                match wparam.0 as i32 {
                    HOTKEY_WS_PREV => switch_workspace_relative(-1),
                    HOTKEY_WS_NEXT => switch_workspace_relative(1),
                    HOTKEY_MOVE_WIN_PREV => move_focused_window_relative(-1),
                    HOTKEY_MOVE_WIN_NEXT => move_focused_window_relative(1),
                    _ => {}
                }
                LRESULT(0)
            }
            WM_TIMER => {
                match wparam.0 {
                    ANIM_TIMER_ID => on_animation_tick(),
                    CLOCK_TIMER_ID => {
                        let primary =
                            STATE.with(|s| s.borrow().as_ref().map(|st| st.primary_bar_hwnd));
                        if let Some(primary) = primary {
                            let _ = InvalidateRect(primary, None, true);
                        }
                        // Explorer re-shows an auto-hidden taskbar on edge
                        // hover or the Win key; while this shell runs it
                        // should stay gone.
                        if let Ok(tray) = FindWindowW(w!("Shell_TrayWnd"), None) {
                            if IsWindowVisible(tray).as_bool() {
                                set_taskbar_windows_visible(false);
                            }
                        }
                    }
                    _ => {}
                }
                LRESULT(0)
            }
            WM_DESTROY => {
                if let Role::Bar { is_primary } = role {
                    unregister_appbar(hwnd);
                    if is_primary {
                        let _ = UnregisterHotKey(hwnd, HOTKEY_WS_PREV);
                        let _ = UnregisterHotKey(hwnd, HOTKEY_WS_NEXT);
                        let _ = UnregisterHotKey(hwnd, HOTKEY_MOVE_WIN_PREV);
                        let _ = UnregisterHotKey(hwnd, HOTKEY_MOVE_WIN_NEXT);
                        // Without a workspace manager running, "parked
                        // off-screen" just means "stranded off-screen" —
                        // bring every tracked window back before going
                        // away. (A hard kill still strands them; the
                        // `top` guard in `park_window`/`unpark_window`
                        // lets the next run recover those.)
                        let tracked: Vec<isize> = STATE.with(|s| {
                            s.borrow()
                                .as_ref()
                                .map(|st| {
                                    st.workspaces
                                        .workspace_ids()
                                        .to_vec()
                                        .into_iter()
                                        .flat_map(|id| st.workspaces.windows_on(id))
                                        .collect()
                                })
                                .unwrap_or_default()
                        });
                        for tracked_hwnd in tracked {
                            unpark_window(HWND(tracked_hwnd as *mut c_void));
                        }
                        set_windows_taskbar_visible(true);
                        restore_work_areas();
                    }
                }
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wparam, lparam),
        }
    }
}

#[cfg(windows)]
fn main() -> groveshell_common::Result<()> {
    imp::main()
}

#[cfg(not(windows))]
fn main() {
    eprintln!("groveshell-ui is Windows-only.");
    std::process::exit(1);
}
