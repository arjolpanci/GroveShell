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
//! This is `groveshell-window-model`'s `ManagedWorkspaceBackend` (ADR-005):
//! GroveShell owns workspace membership itself and hides/shows real windows
//! via `ShowWindow(SW_HIDE)`/`ShowWindow(SW_SHOWNA)` rather than delegating
//! to Windows' own virtual desktops. Per §8.3, workspaces are dynamic and
//! global across all monitors (not yet per-monitor — a later, explicitly
//! out-of-scope enhancement): the app always starts with at least two, a
//! new empty one appears once the last is occupied, and redundant trailing
//! empties collapse back down to the floor of two. The pure
//! assignment/growth logic lives in `groveshell_window_model::workspace`;
//! this module only does the Windows-specific parts: hiding/showing real
//! windows, and the Activities overview's *carousel* — one full page per
//! workspace, laid out side by side, that you drag horizontally to switch
//! (see `carousel_offset`/`CarouselDrag`/`CarouselAnim`). Only the current
//! workspace's page ever gets live DWM thumbnails, since a window hidden on
//! an inactive workspace has no live pixel content to preview; other pages
//! render static title-only placeholder chips that upgrade to live
//! thumbnails the moment `commit_workspace_switch` actually shows that
//! workspace's windows again. Global hotkeys (`Ctrl+Alt+Left/Right` to
//! switch, `+Shift` to also move the focused window) work whether or not
//! the overview is open.
//!
//! Deliberately out of scope for this slice: *per-monitor* workspace sets,
//! Windows' own Task View virtual desktops (a separate, unrelated
//! mechanism — DWM already cloaks windows on an inactive one, and this
//! shell's cloaked-window filter already excludes those from every
//! workspace's page), hot corners, per-monitor DPI scaling, and live
//! updates while the overview is open (a window opening/closing mid-session
//! isn't picked up until the overview is reopened).

#[cfg(windows)]
mod imp {
    use std::cell::RefCell;
    use std::ffi::c_void;
    use std::time::{Duration, Instant};

    use groveshell_common::{Error, Result};
    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::{BOOL, COLORREF, HWND, LPARAM, LRESULT, RECT, TRUE, WPARAM};
    use windows::Win32::Graphics::Dwm::{
        DwmRegisterThumbnail, DwmUnregisterThumbnail, DwmUpdateThumbnailProperties,
        DWM_THUMBNAIL_PROPERTIES, DWM_TNP_OPACITY, DWM_TNP_RECTDESTINATION, DWM_TNP_VISIBLE,
    };
    use windows::Win32::Graphics::Gdi::{
        BeginPaint, CreateSolidBrush, DeleteObject, DrawTextW, Ellipse, EndPaint, EnumDisplayMonitors,
        FillRect, GetMonitorInfoW, InvalidateRect, SelectObject, SetBkMode, SetTextColor,
        DRAW_TEXT_FORMAT, DT_CENTER, DT_END_ELLIPSIS, DT_SINGLELINE, DT_VCENTER, HDC, HMONITOR,
        MONITORINFO, PAINTSTRUCT, TRANSPARENT,
    };
    use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
    use windows::Win32::Media::Audio::{eConsole, eRender, IMMDeviceEnumerator, MMDeviceEnumerator};
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_ALL, COINIT_APARTMENTTHREADED};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};
    use windows::Win32::System::SystemInformation::GetLocalTime;
    use windows::Win32::System::SystemServices::MK_LBUTTON;
    use windows::Win32::UI::HiDpi::{SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2};
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        RegisterHotKey, SetFocus, UnregisterHotKey, MOD_ALT, MOD_CONTROL, MOD_SHIFT, VK_ESCAPE,
        VK_LEFT, VK_RIGHT,
    };
    use windows::Win32::UI::Shell::{
        SHAppBarMessage, ABE_TOP, ABM_NEW, ABM_QUERYPOS, ABM_REMOVE, ABM_SETPOS, APPBARDATA,
    };
    use windows::Win32::UI::WindowsAndMessaging::*;

    use groveshell_window_model::workspace::{WorkspaceId, WorkspaceTracker};

    /// Half of the original 32px guess — the Windows taskbar itself is
    /// ~40px, but this shell is meant to feel closer to GNOME's slim bar;
    /// 50% taller than the first cut (16px) to fit the clock and quick
    /// settings labels comfortably.
    const BAR_HEIGHT: i32 = 24;
    /// Hit-test region for the painted (not native controls — there isn't
    /// enough vertical room in the bar for real button chrome) bar labels.
    const ACTIVITIES_LABEL_X: i32 = 8;
    const ACTIVITIES_LABEL_WIDTH: i32 = 72;
    const CLOCK_LABEL_WIDTH: i32 = 130;
    const QS_LABEL_WIDTH: i32 = 170;
    const QS_LABEL_MARGIN: i32 = 8;

    /// How much of its own size a workspace card (and everything on it)
    /// shrinks to in the Activities overview — GNOME-style "zoom out",
    /// each monitor toward its own center.
    const OVERVIEW_SCALE: f64 = 0.6;
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

    #[derive(Clone, Copy)]
    struct MonitorInfo {
        /// Full monitor bounds in virtual-screen coordinates (which can be
        /// negative — the primary monitor anchors the origin, so a monitor
        /// to its left or above it has negative coordinates).
        rect: RECT,
        is_primary: bool,
    }

    /// One monitor's top-level bar window plus the rect the AppBar system
    /// actually assigned it.
    struct BarWindow {
        hwnd: HWND,
        rect: RECT,
        is_primary: bool,
    }

    /// One window's overview preview plus its animation endpoints, all in
    /// `overview_hwnd`-local, **page-local** (pre-carousel-shift) client-area
    /// coordinates — the horizontal carousel offset is applied on top of
    /// `current` at the moment a rect is pushed to DWM, painted, or
    /// hit-tested (see `displayed_rect`), never baked into `current` itself.
    struct ThumbAnim {
        /// `Some` for a live DWM thumbnail; `None` renders as a static
        /// title-only placeholder instead. Only the current workspace's
        /// page ever starts with a live thumbnail — a window hidden on an
        /// inactive workspace has no live pixel content to preview — but a
        /// placeholder is upgraded to live the moment
        /// `commit_workspace_switch` actually shows its window again.
        thumbnail: Option<isize>,
        hwnd: HWND,
        /// Used only for placeholder rendering.
        title: String,
        /// Which carousel page (workspace index, fixed for the lifetime of
        /// one overview session) this belongs to.
        page: usize,
        /// Animation start rect for whichever transition is currently
        /// running (real position when opening, scaled position when
        /// closing); equal to `to` for every non-current page, since those
        /// start already "zoomed out" with no opening animation of their
        /// own.
        from: RECT,
        /// Animation end rect (the inverse of `from`).
        to: RECT,
        /// Last computed rect — while `Open`, this equals `to` from the
        /// opening animation and is what's hit-tested against clicks (after
        /// applying the current carousel shift).
        current: RECT,
    }

    /// A monitor's "workspace card" background rect, animated the same way
    /// as `ThumbAnim` but with no DWM handle — it's plain GDI fill, redrawn
    /// via `InvalidateRect` on every animation tick.
    struct CardAnim {
        page: usize,
        from: RECT,
        to: RECT,
        current: RECT,
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
        Opening {
            started: Instant,
            thumbs: Vec<ThumbAnim>,
            cards: Vec<CardAnim>,
        },
        /// Idle, fully zoomed out; `thumbs[].current` is stable and is what
        /// clicks are hit-tested against.
        Open {
            thumbs: Vec<ThumbAnim>,
            cards: Vec<CardAnim>,
        },
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
            monitors.push(MonitorInfo {
                rect: RECT {
                    left: 0,
                    top: 0,
                    right: w,
                    bottom: h,
                },
                is_primary: true,
            });
        }
        monitors
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
            monitors.push(MonitorInfo {
                rect: info.rcMonitor,
                is_primary: info.dwFlags & MONITORINFOF_PRIMARY != 0,
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
            let monitors = enumerate_monitors();
            let mut bars = Vec::new();
            for monitor in &monitors {
                let width = monitor.rect.right - monitor.rect.left;
                let bar_hwnd = CreateWindowExW(
                    WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                    w!("GroveShellBar"),
                    w!("GroveShell"),
                    WS_POPUP | WS_VISIBLE,
                    monitor.rect.left,
                    monitor.rect.top,
                    width,
                    BAR_HEIGHT,
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
                    register_appbar(bar_hwnd, monitor.rect.left, monitor.rect.top, width, BAR_HEIGHT);
                let _ = MoveWindow(
                    bar_hwnd,
                    bar_rect.left,
                    bar_rect.top,
                    bar_rect.right - bar_rect.left,
                    bar_rect.bottom - bar_rect.top,
                    true,
                );

                bars.push(BarWindow {
                    hwnd: bar_hwnd,
                    rect: bar_rect,
                    is_primary: monitor.is_primary,
                });
            }

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
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
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
                    workspaces: WorkspaceTracker::new(),
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
                let bar_width = STATE
                    .with(|s| {
                        s.borrow()
                            .as_ref()
                            .map(|st| st.primary_bar_rect.right - st.primary_bar_rect.left)
                    })
                    .unwrap_or(0);

                SetBkMode(hdc, TRANSPARENT);
                SetTextColor(hdc, COLORREF(0x00E0E0E0));

                let format = DT_SINGLELINE | DT_VCENTER | DT_CENTER;
                draw_text_in(
                    hdc,
                    RECT {
                        left: ACTIVITIES_LABEL_X,
                        top: 0,
                        right: ACTIVITIES_LABEL_X + ACTIVITIES_LABEL_WIDTH,
                        bottom: BAR_HEIGHT,
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
                let dot_mid_y = BAR_HEIGHT / 2;
                let filled_brush = CreateSolidBrush(COLORREF(0x00E0E0E0));
                let empty_brush = CreateSolidBrush(COLORREF(0x00606060));
                for i in 0..workspace_count {
                    let cx = WS_DOTS_X + i as i32 * WS_DOT_SLOT_WIDTH + WS_DOT_SLOT_WIDTH / 2;
                    let brush = if i == current_index { filled_brush } else { empty_brush };
                    let previous = SelectObject(hdc, brush);
                    let _ = Ellipse(
                        hdc,
                        cx - WS_DOT_RADIUS,
                        dot_mid_y - WS_DOT_RADIUS,
                        cx + WS_DOT_RADIUS,
                        dot_mid_y + WS_DOT_RADIUS,
                    );
                    SelectObject(hdc, previous);
                }
                let _ = DeleteObject(filled_brush);
                let _ = DeleteObject(empty_brush);

                let clock_x = bar_width / 2 - CLOCK_LABEL_WIDTH / 2;
                draw_text_in(
                    hdc,
                    RECT {
                        left: clock_x,
                        top: 0,
                        right: clock_x + CLOCK_LABEL_WIDTH,
                        bottom: BAR_HEIGHT,
                    },
                    &clock_text(),
                    format,
                );

                let qs_x = bar_width - QS_LABEL_WIDTH - QS_LABEL_MARGIN;
                draw_text_in(
                    hdc,
                    RECT {
                        left: qs_x,
                        top: 0,
                        right: qs_x + QS_LABEL_WIDTH,
                        bottom: BAR_HEIGHT,
                    },
                    &quick_settings_label_text(),
                    format,
                );
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

    /// Draws every workspace card at its current (possibly mid-animation)
    /// rect. This is a flat placeholder "desktop" fill, not the real
    /// wallpaper: reliably capturing it would mean decoding whatever
    /// format/location Windows currently stores it in (JPEG/PNG, sometimes
    /// per-monitor, sometimes a transcoded cache file), which needs GDI+/WIC
    /// rather than classic GDI — left as a follow-up. This still fixes the
    /// actual complaint it's standing in for: every card now has a fixed,
    /// visible boundary, so a small window inside it doesn't look like it's
    /// floating in empty space.
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
            let page_w = page_width();
            let cards = cards
                .iter()
                .map(|c| displayed_rect(c.current, c.page, st.carousel_offset, page_w))
                .collect::<Vec<_>>();
            let placeholders = thumbs
                .iter()
                .filter(|th| th.thumbnail.is_none())
                .map(|th| (displayed_rect(th.current, th.page, st.carousel_offset, page_w), th.title.clone()))
                .collect::<Vec<_>>();
            Some((cards, placeholders))
        });

        // SAFETY: `hwnd` is the window currently processing `WM_PAINT`.
        unsafe {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            if let Some((cards, placeholders)) = content {
                let brush = CreateSolidBrush(COLORREF(0x00805030));
                for rect in &cards {
                    FillRect(hdc, rect, brush);
                }
                let _ = DeleteObject(brush);

                // Placeholder chips: windows on a workspace that isn't
                // current have no live pixel content (they're hidden), so
                // this is just their last-known title, not a preview.
                let chip_brush = CreateSolidBrush(COLORREF(0x00404040));
                SetBkMode(hdc, TRANSPARENT);
                SetTextColor(hdc, COLORREF(0x00E0E0E0));
                for (rect, title) in &placeholders {
                    FillRect(hdc, rect, chip_brush);
                    draw_text_in(
                        hdc,
                        *rect,
                        title,
                        DT_SINGLELINE | DT_VCENTER | DT_CENTER | DT_END_ELLIPSIS,
                    );
                }
                let _ = DeleteObject(chip_brush);
            }
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

    fn lerp(a: i32, b: i32, t: f64) -> i32 {
        (a as f64 + (b as f64 - a as f64) * t).round() as i32
    }

    fn lerp_rect(from: RECT, to: RECT, t: f64) -> RECT {
        RECT {
            left: lerp(from.left, to.left, t),
            top: lerp(from.top, to.top, t),
            right: lerp(from.right, to.right, t),
            bottom: lerp(from.bottom, to.bottom, t),
        }
    }

    /// Scales `r` toward `(pivot_x, pivot_y)` by factor `s` — the "zoom
    /// out toward the center of its own workspace card" transform applied
    /// uniformly to a card and everything on it, as a group, rather than
    /// rearranging windows into a grid.
    fn scale_about(r: RECT, pivot_x: i32, pivot_y: i32, s: f64) -> RECT {
        let scale = |v: i32, pivot: i32| pivot + ((v - pivot) as f64 * s).round() as i32;
        RECT {
            left: scale(r.left, pivot_x),
            top: scale(r.top, pivot_y),
            right: scale(r.right, pivot_x),
            bottom: scale(r.bottom, pivot_y),
        }
    }

    /// The overview spans the full virtual screen (see `open_overview`), so
    /// that's also each carousel page's width — pages sit side by side,
    /// one virtual-screen-width apart. Recomputed on demand rather than
    /// cached: cheap, and consistent with the rest of this module not
    /// handling live display-topology changes.
    fn page_width() -> i32 {
        // SAFETY: no preconditions.
        unsafe { GetSystemMetrics(SM_CXVIRTUALSCREEN) }
    }

    fn shift_x(r: RECT, dx: i32) -> RECT {
        RECT {
            left: r.left + dx,
            top: r.top,
            right: r.right + dx,
            bottom: r.bottom,
        }
    }

    /// Translates a page-local rect by the carousel's current horizontal
    /// scroll: page `page` sits at `(page - offset) * page_width` pixels
    /// from its own local origin. Applied only at the moment a rect is
    /// pushed to DWM, painted, or hit-tested — never baked back into
    /// `ThumbAnim`/`CardAnim::current`, so the zoom animation math in
    /// `tick_thumbs`/`tick_cards` never has to know about the carousel.
    fn displayed_rect(base: RECT, page: usize, offset: f64, page_width: i32) -> RECT {
        let dx = ((page as f64 - offset) * page_width as f64).round() as i32;
        shift_x(base, dx)
    }

    fn set_thumb_rect(thumbnail: isize, rect: RECT) {
        let props = DWM_THUMBNAIL_PROPERTIES {
            dwFlags: DWM_TNP_RECTDESTINATION | DWM_TNP_VISIBLE | DWM_TNP_OPACITY,
            rcDestination: rect,
            opacity: 255,
            fVisible: windows::Win32::Foundation::TRUE,
            ..Default::default()
        };
        // SAFETY: `thumbnail` is a handle registered by `DwmRegisterThumbnail`
        // earlier in the same overview session and not yet unregistered.
        unsafe {
            let _ = DwmUpdateThumbnailProperties(thumbnail, &props);
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
    /// currently-visible-but-untracked window to the current workspace, and
    /// drops assignments for windows that no longer exist — since this
    /// process has no live window-create/destroy event tracking yet
    /// (Phase 1's `SetWinEventHook` follow-up); every workspace-changing
    /// action re-syncs first instead. Returns the live snapshot taken to do
    /// this, for callers that also need it (building the current page).
    fn sync_workspaces() -> Vec<groveshell_window_model::WindowRecord> {
        let live = groveshell_window_model::snapshot();
        STATE.with(|s| {
            if let Some(state) = s.borrow_mut().as_mut() {
                state.workspaces.observe_active(live.iter().map(|w| w.hwnd));
                state.workspaces.prune(groveshell_window_model::is_alive);
            }
        });
        live
    }

    /// Starts the "zoom out" animation: for every workspace, for every
    /// monitor, registers its workspace card (see module docs) and a
    /// preview for every window assigned to it, then kicks off a timer to
    /// animate the *current* workspace's page down to `OVERVIEW_SCALE`
    /// around its own monitors' centers (every other page starts already
    /// zoomed out, off to the side — see the module docs on the carousel).
    /// No-op if the overview isn't currently `Closed` (already open or
    /// mid-animation).
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

        let live = sync_workspaces();

        // SAFETY: `overview_hwnd` is a valid, process-lifetime window.
        let mut overview_screen_rect = RECT::default();
        unsafe {
            let _ = GetWindowRect(overview_hwnd, &mut overview_screen_rect);
        }
        let origin_x = overview_screen_rect.left;
        let origin_y = overview_screen_rect.top;

        struct MonitorCard {
            /// This monitor's desktop area (below its own bar), in
            /// overview-local coordinates.
            rect: RECT,
            pivot: (i32, i32),
        }
        let monitor_cards: Vec<MonitorCard> = enumerate_monitors()
            .iter()
            .map(|m| {
                let local = RECT {
                    left: m.rect.left - origin_x,
                    top: m.rect.top - origin_y + BAR_HEIGHT,
                    right: m.rect.right - origin_x,
                    bottom: m.rect.bottom - origin_y,
                };
                let pivot = ((local.left + local.right) / 2, (local.top + local.bottom) / 2);
                MonitorCard { rect: local, pivot }
            })
            .collect();

        let (workspace_ids, current_pos): (Vec<WorkspaceId>, usize) = STATE.with(|s| {
            s.borrow()
                .as_ref()
                .map(|st| (st.workspaces.workspace_ids().to_vec(), st.workspaces.current_index()))
                .unwrap_or_default()
        });

        let page_w = page_width();
        let mut cards = Vec::new();
        let mut thumbs = Vec::new();
        let mut live_opt = Some(live);

        for (page, &ws_id) in workspace_ids.iter().enumerate() {
            let is_current_page = page == current_pos;

            for mc in &monitor_cards {
                let to = scale_about(mc.rect, mc.pivot.0, mc.pivot.1, OVERVIEW_SCALE);
                let from = if is_current_page { mc.rect } else { to };
                cards.push(CardAnim { page, from, to, current: from });
            }

            // Only the current workspace's windows are ever actually
            // visible (everything else is workspace-hidden); reuse the
            // snapshot already taken by `sync_workspaces` for it rather
            // than re-enumerating.
            let windows: Vec<groveshell_window_model::WindowRecord> = if is_current_page {
                live_opt.take().unwrap_or_default()
            } else {
                STATE
                    .with(|s| {
                        s.borrow()
                            .as_ref()
                            .map(|st| st.workspaces.windows_on(ws_id))
                            .unwrap_or_default()
                    })
                    .into_iter()
                    .filter_map(groveshell_window_model::describe)
                    .collect()
            };

            for window in windows {
                let source = HWND(window.hwnd as *mut c_void);
                let from_real = RECT {
                    left: window.rect.left - origin_x,
                    top: window.rect.top - origin_y,
                    right: window.rect.right - origin_x,
                    bottom: window.rect.bottom - origin_y,
                };

                // Scale this window in lockstep with whichever monitor's
                // card contains its center — not one shared pivot for the
                // whole multi-monitor span, which would make two monitors'
                // windows collide toward a single point somewhere between
                // them. Falls back to the first monitor if the center is
                // somehow outside all of them (e.g. a monitor was just
                // disconnected).
                let center_x = (from_real.left + from_real.right) / 2;
                let center_y = (from_real.top + from_real.bottom) / 2;
                let owner = monitor_cards
                    .iter()
                    .find(|mc| {
                        center_x >= mc.rect.left
                            && center_x < mc.rect.right
                            && center_y >= mc.rect.top
                            && center_y < mc.rect.bottom
                    })
                    .unwrap_or(&monitor_cards[0]);
                let zoomed = scale_about(from_real, owner.pivot.0, owner.pivot.1, OVERVIEW_SCALE);

                let (from, initial) = if is_current_page {
                    (from_real, from_real)
                } else {
                    (zoomed, zoomed)
                };

                // A hidden (inactive-workspace) window has no live pixel
                // content to preview — see the module docs — so only the
                // current page ever gets a real DWM thumbnail here.
                // SAFETY: `overview_hwnd` and `source` are both live
                // windows; `source` was enumerated moments ago and may
                // have closed since, in which case this simply fails.
                let thumbnail = if is_current_page {
                    unsafe { DwmRegisterThumbnail(overview_hwnd, source) }.ok()
                } else {
                    None
                };

                if let Some(handle) = thumbnail {
                    // Show it immediately at "from" — exactly where the
                    // real window currently sits — so the animation reads
                    // as that window shrinking away, not popping in from
                    // nowhere.
                    set_thumb_rect(handle, displayed_rect(initial, page, current_pos as f64, page_w));
                }

                thumbs.push(ThumbAnim {
                    thumbnail,
                    hwnd: source,
                    title: window.title,
                    page,
                    from,
                    to: zoomed,
                    current: initial,
                });
            }
        }

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
            let _ = ShowWindow(overview_hwnd, SW_SHOW);
            let _ = SetForegroundWindow(overview_hwnd);
            let _ = SetFocus(overview_hwnd);
            SetTimer(overview_hwnd, ANIM_TIMER_ID, ANIM_TIMER_INTERVAL_MS, None);
        }
    }

    /// Starts the reverse animation from wherever the thumbnails/cards
    /// currently are back to their real positions, then hides the overview
    /// and focuses `focus_after` (or restores whatever was focused before
    /// Activities was opened, if `None`). Works whether the overview is
    /// currently `Open` (idle) or still `Opening` (interrupts it smoothly
    /// from its current position — no-op only if already `Closed` or
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
                    let thumbs = thumbs
                        .into_iter()
                        .map(|th| ThumbAnim {
                            from: th.current,
                            to: th.from,
                            current: th.current,
                            ..th
                        })
                        .collect();
                    let cards = cards
                        .into_iter()
                        .map(|c| CardAnim {
                            from: c.current,
                            to: c.from,
                            current: c.current,
                            ..c
                        })
                        .collect();
                    state.overview = OverviewMode::Closing {
                        started: Instant::now(),
                        thumbs,
                        cards,
                        focus_after,
                    };
                    // Any in-progress carousel drag/slide is moot once
                    // we're zooming back out.
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
    /// painted regions it landed in (see `paint_bar` for the same layout).
    fn on_bar_click(x: i32) {
        let bar_width = STATE.with(|s| {
            s.borrow()
                .as_ref()
                .map(|st| st.primary_bar_rect.right - st.primary_bar_rect.left)
        });
        let Some(bar_width) = bar_width else {
            return;
        };

        if (ACTIVITIES_LABEL_X..ACTIVITIES_LABEL_X + ACTIVITIES_LABEL_WIDTH).contains(&x) {
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
        let dots_width = workspace_count as i32 * WS_DOT_SLOT_WIDTH;
        if (WS_DOTS_X..WS_DOTS_X + dots_width).contains(&x) {
            let index = ((x - WS_DOTS_X) / WS_DOT_SLOT_WIDTH) as usize;
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

        let clock_x = bar_width / 2 - CLOCK_LABEL_WIDTH / 2;
        if (clock_x..clock_x + CLOCK_LABEL_WIDTH).contains(&x) {
            toggle_calendar();
            return;
        }

        let qs_x = bar_width - QS_LABEL_WIDTH - QS_LABEL_MARGIN;
        if (qs_x..bar_width - QS_LABEL_MARGIN).contains(&x) {
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
            let page_w = page_width();
            let thumb_hit = thumbs.iter().find_map(|th| {
                let r = displayed_rect(th.current, th.page, st.carousel_offset, page_w);
                (x >= r.left && x < r.right && y >= r.top && y < r.bottom)
                    .then_some(OverviewHit::Window { page: th.page, hwnd: th.hwnd })
            });
            let hit = thumb_hit.or_else(|| {
                cards.iter().find_map(|c| {
                    let r = displayed_rect(c.current, c.page, st.carousel_offset, page_w);
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

    /// Follows the pointer 1:1 while a carousel drag is active — content
    /// moves with the cursor, so dragging right reveals the *previous*
    /// (lower-index) workspace, matching how a touch/trackpad carousel
    /// feels.
    fn on_overview_drag_move(x: i32) {
        let overview_hwnd = STATE.with(|s| {
            let mut state_ref = s.borrow_mut();
            let state = state_ref.as_mut()?;
            let workspace_count = state.workspaces.workspace_ids().len();
            let drag = state.carousel_drag.as_mut()?;
            let delta_px = x - drag.start_x;
            drag.max_delta = drag.max_delta.max(delta_px.abs());
            let page_w = page_width().max(1);
            let raw_offset = drag.start_offset - delta_px as f64 / page_w as f64;
            state.carousel_offset = raw_offset.clamp(0.0, (workspace_count.max(1) - 1) as f64);
            Some(state.overview_hwnd)
        });
        if let Some(overview_hwnd) = overview_hwnd {
            push_carousel_positions(overview_hwnd);
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

    /// Commits to the workspace at `target_index`: a real `ShowWindow`
    /// hide/show swap, so the desktop actually reflects the new current
    /// workspace immediately, even if the overview stays open — GNOME
    /// switches live as soon as you land on a workspace in the overview
    /// too, rather than waiting for it to close. No-op if `target_index` is
    /// already current.
    fn commit_workspace_switch(target_index: usize) {
        let switch = STATE.with(|s| {
            let mut state_ref = s.borrow_mut();
            let state = state_ref.as_mut()?;
            let (from_id, to_id) = state.workspaces.switch_to_index(target_index)?;
            Some((state.workspaces.windows_on(from_id), state.workspaces.windows_on(to_id)))
        });
        let Some((hide, show)) = switch else {
            return;
        };

        // SAFETY: each hwnd was tracked as a real, previously-eligible
        // window; if it's since closed these are documented no-ops.
        unsafe {
            for hwnd in &hide {
                let _ = ShowWindow(HWND(*hwnd as *mut c_void), SW_HIDE);
            }
            for hwnd in &show {
                let _ = ShowWindow(HWND(*hwnd as *mut c_void), SW_SHOWNA);
            }
        }

        upgrade_placeholder_thumbnails(&show);
        refresh_bar_indicator();
    }

    /// Upgrades any overview placeholder chip whose window is in
    /// `now_visible` to a live DWM thumbnail, now that
    /// `commit_workspace_switch` has actually shown it again. No-op if the
    /// overview isn't open.
    fn upgrade_placeholder_thumbnails(now_visible: &[isize]) {
        let (overview_hwnd, page_w, offset, upgrades) = STATE.with(|s| {
            let state_ref = s.borrow();
            let none = (HWND(std::ptr::null_mut()), 0, 0.0, Vec::new());
            let Some(st) = state_ref.as_ref() else {
                return none;
            };
            let thumbs = match &st.overview {
                OverviewMode::Opening { thumbs, .. }
                | OverviewMode::Open { thumbs, .. }
                | OverviewMode::Closing { thumbs, .. } => thumbs,
                OverviewMode::Closed => return none,
            };
            let upgrades = thumbs
                .iter()
                .enumerate()
                .filter(|(_, th)| th.thumbnail.is_none() && now_visible.contains(&(th.hwnd.0 as isize)))
                .map(|(i, th)| (i, th.hwnd, th.page, th.current))
                .collect::<Vec<_>>();
            (st.overview_hwnd, page_width(), st.carousel_offset, upgrades)
        });

        if upgrades.is_empty() {
            return;
        }

        for (index, hwnd, page, current_rect) in upgrades {
            // SAFETY: `overview_hwnd` is a valid, process-lifetime window;
            // `hwnd` was just made visible by `commit_workspace_switch`.
            let Ok(handle) = (unsafe { DwmRegisterThumbnail(overview_hwnd, hwnd) }) else {
                continue;
            };
            set_thumb_rect(handle, displayed_rect(current_rect, page, offset, page_w));
            STATE.with(|s| {
                if let Some(state) = s.borrow_mut().as_mut() {
                    let thumbs = match &mut state.overview {
                        OverviewMode::Opening { thumbs, .. }
                        | OverviewMode::Open { thumbs, .. }
                        | OverviewMode::Closing { thumbs, .. } => Some(thumbs),
                        OverviewMode::Closed => None,
                    };
                    if let Some(th) = thumbs.and_then(|t| t.get_mut(index)) {
                        th.thumbnail = Some(handle);
                    }
                }
            });
        }

        // SAFETY: `overview_hwnd` is a valid, process-lifetime window.
        unsafe {
            let _ = InvalidateRect(overview_hwnd, None, true);
        }
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

    /// Pushes every live thumbnail's current carousel-shifted rect to DWM
    /// and repaints the overview (cards and placeholder chips are read
    /// fresh from state at paint time, so a plain invalidate is enough for
    /// those). Called after anything changes `carousel_offset` — a drag
    /// move, or an animation tick.
    fn push_carousel_positions(overview_hwnd: HWND) {
        let updates = STATE.with(|s| {
            let state_ref = s.borrow();
            let Some(st) = state_ref.as_ref() else {
                return Vec::new();
            };
            let thumbs = match &st.overview {
                OverviewMode::Opening { thumbs, .. }
                | OverviewMode::Open { thumbs, .. }
                | OverviewMode::Closing { thumbs, .. } => thumbs,
                OverviewMode::Closed => return Vec::new(),
            };
            let page_w = page_width();
            thumbs
                .iter()
                .filter_map(|th| {
                    th.thumbnail
                        .map(|h| (h, displayed_rect(th.current, th.page, st.carousel_offset, page_w)))
                })
                .collect()
        });
        for (thumbnail, rect) in updates {
            set_thumb_rect(thumbnail, rect);
        }
        // SAFETY: `overview_hwnd` is a valid, process-lifetime window.
        unsafe {
            let _ = InvalidateRect(overview_hwnd, None, true);
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

    /// `Ctrl+Alt+Shift+Left/Right` — moves the foreground window to the
    /// adjacent workspace and hides it immediately (the user stays on the
    /// current workspace; only the window leaves). No-op if nothing
    /// eligible is focused (including any of this shell's own windows).
    fn move_focused_window_relative(delta: i32) {
        // SAFETY: no preconditions.
        let fg = unsafe { GetForegroundWindow() };
        if fg.0.is_null() || role_of(fg) != Role::Other {
            return;
        }
        sync_workspaces();
        let hwnd = fg.0 as isize;
        let moved_away = STATE.with(|s| {
            let mut state_ref = s.borrow_mut();
            let state = state_ref.as_mut()?;
            let current = state.workspaces.current_id();
            let new_id = state.workspaces.move_window_relative(hwnd, delta)?;
            Some(new_id != current)
        });
        if moved_away == Some(true) {
            // SAFETY: `fg` was just confirmed live via `GetForegroundWindow`.
            unsafe {
                let _ = ShowWindow(fg, SW_HIDE);
            }
        }
        refresh_bar_indicator();
    }

    /// Advances whichever animation (`Opening`/`Closing` zoom, and/or an
    /// independent in-flight carousel slide — these can run at the same
    /// time, or a carousel slide can run on its own while the overview
    /// just sits `Open`) by one tick, or finalizes it once it reaches the
    /// end.
    fn on_animation_tick() {
        enum Completion {
            Opened,
            Closed {
                focus_after: Option<HWND>,
                thumbs: Vec<ThumbAnim>,
            },
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
            let (new_mode, zoom_running, completion) = match mode {
                OverviewMode::Opening {
                    started,
                    mut thumbs,
                    mut cards,
                } => {
                    let t = progress(started);
                    let eased = ease_out(t);
                    tick_thumbs(&mut thumbs, eased);
                    tick_cards(&mut cards, eased);
                    if t >= 1.0 {
                        (OverviewMode::Open { thumbs, cards }, false, Some(Completion::Opened))
                    } else {
                        (OverviewMode::Opening { started, thumbs, cards }, true, None)
                    }
                }
                OverviewMode::Closing {
                    started,
                    mut thumbs,
                    mut cards,
                    focus_after,
                } => {
                    let t = progress(started);
                    let eased = ease_out(t);
                    tick_thumbs(&mut thumbs, eased);
                    tick_cards(&mut cards, eased);
                    if t >= 1.0 {
                        (OverviewMode::Closed, false, Some(Completion::Closed { focus_after, thumbs }))
                    } else {
                        (
                            OverviewMode::Closing { started, thumbs, cards, focus_after },
                            true,
                            None,
                        )
                    }
                }
                other => (other, false, None),
            };
            state.overview = new_mode;

            let carousel_close_after = if carousel_done { state.carousel_close_after.take() } else { None };
            let keep_timer = zoom_running || state.carousel_anim.is_some();
            Some((state.overview_hwnd, completion, carousel_close_after, keep_timer))
        });

        let Some((overview_hwnd, completion, carousel_close_after, keep_timer)) = result else {
            return;
        };

        // Pushes every live thumbnail's rect (carousel-shifted, using
        // whatever `carousel_offset` ended up at above) and repaints for
        // the cards/placeholders, whether or not anything about the zoom
        // animation changed this tick.
        push_carousel_positions(overview_hwnd);

        if !keep_timer {
            // SAFETY: `overview_hwnd` is a valid, process-lifetime window.
            unsafe {
                let _ = KillTimer(overview_hwnd, ANIM_TIMER_ID);
            }
        }

        if let Some(completion) = completion {
            match completion {
                Completion::Opened => {}
                Completion::Closed { focus_after, thumbs } => {
                    for th in &thumbs {
                        if let Some(handle) = th.thumbnail {
                            // SAFETY: registered in `open_overview` or
                            // `upgrade_placeholder_thumbnails` and not yet
                            // unregistered.
                            unsafe {
                                let _ = DwmUnregisterThumbnail(handle);
                            }
                        }
                    }
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

    fn tick_thumbs(thumbs: &mut [ThumbAnim], t: f64) {
        for th in thumbs.iter_mut() {
            th.current = lerp_rect(th.from, th.to, t);
        }
    }

    fn tick_cards(cards: &mut [CardAnim], t: f64) {
        for card in cards.iter_mut() {
            card.current = lerp_rect(card.from, card.to, t);
        }
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
                        on_bar_click(x);
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
