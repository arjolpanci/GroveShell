//! `groveshell-ui`: first-iteration shell UI. Per `docs/PROJECT_PLAN.md`
//! §10.1/§10.2, creates a single top bar (Activities affordance only —
//! clock, workspace indicator, and system indicators are later scope) and
//! a GNOME-style Activities overview: clicking Activities zooms the whole
//! desktop out to ~60% into the middle of the overview, using live DWM
//! thumbnails (`DwmRegisterThumbnail`) of the real open windows — not a
//! static list — so the overview is a true, animated representation of the
//! current desktop. Clicking a thumbnail reverses the animation back to
//! that window's real position and focuses it. The bar reserves its strip
//! of the work area via the AppBar API (`SHAppBarMessage`), the same
//! mechanism the Windows taskbar uses, so maximized windows and desktop
//! icon layout respect it instead of being covered.
//!
//! Deliberately out of scope for this slice: multiple workspaces (there is
//! only ever "the current desktop" right now), hot corners, hotkeys, and
//! live updates while the overview is open (a window closing mid-overview
//! just leaves a stale thumbnail until the overview is reopened).

#[cfg(windows)]
mod imp {
    use std::cell::RefCell;
    use std::ffi::c_void;
    use std::time::{Duration, Instant};

    use groveshell_common::{Error, Result};
    use windows::core::{w, PCWSTR};
    use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
    use windows::Win32::Graphics::Dwm::{
        DwmRegisterThumbnail, DwmUnregisterThumbnail, DwmUpdateThumbnailProperties,
        DWM_THUMBNAIL_PROPERTIES, DWM_TNP_OPACITY, DWM_TNP_RECTDESTINATION, DWM_TNP_VISIBLE,
    };
    use windows::Win32::Graphics::Gdi::{
        BeginPaint, CreateSolidBrush, DrawTextW, EndPaint, InvalidateRect, SetBkMode,
        SetTextColor, DT_CENTER, DT_SINGLELINE, DT_VCENTER, PAINTSTRUCT, TRANSPARENT,
    };
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::Input::KeyboardAndMouse::{SetFocus, VK_ESCAPE};
    use windows::Win32::UI::Shell::{
        SHAppBarMessage, ABE_TOP, ABM_NEW, ABM_QUERYPOS, ABM_REMOVE, ABM_SETPOS, APPBARDATA,
    };
    use windows::Win32::UI::WindowsAndMessaging::*;

    /// Half of the original 32px guess — the Windows taskbar itself is
    /// ~40px, but this shell is meant to feel closer to GNOME's slim bar.
    const BAR_HEIGHT: i32 = 16;
    /// Hit-test region for the painted (not a native control — there isn't
    /// enough vertical room in a 16px bar for one) "Activities" label.
    const ACTIVITIES_LABEL_X: i32 = 8;
    const ACTIVITIES_LABEL_WIDTH: i32 = 72;

    /// How much of its original size the whole desktop shrinks to in the
    /// overview — GNOME-style "zoom out", not a per-window grid layout.
    const OVERVIEW_SCALE: f64 = 0.6;
    const ANIM_DURATION: Duration = Duration::from_millis(250);
    const ANIM_TIMER_ID: usize = 1;
    const ANIM_TIMER_INTERVAL_MS: u32 = 16;

    /// One window's live DWM thumbnail plus its animation endpoints, all in
    /// `overview_hwnd`-local (client-area) coordinates.
    struct ThumbAnim {
        thumbnail: isize,
        hwnd: HWND,
        /// Animation start rect for whichever transition is currently
        /// running (real position when opening, scaled position when
        /// closing).
        from: RECT,
        /// Animation end rect (the inverse of `from`).
        to: RECT,
        /// Last computed rect — while `Open`, this equals `to` from the
        /// opening animation and is what's hit-tested against clicks.
        current: RECT,
    }

    enum OverviewMode {
        Closed,
        Opening {
            started: Instant,
            thumbs: Vec<ThumbAnim>,
        },
        /// Idle, fully zoomed out; `thumbs[].current` is stable and is what
        /// clicks are hit-tested against.
        Open {
            thumbs: Vec<ThumbAnim>,
        },
        Closing {
            started: Instant,
            thumbs: Vec<ThumbAnim>,
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
        bar_hwnd: HWND,
        overview_hwnd: HWND,
        overview: OverviewMode,
        /// Captured right before opening the overview, so cancelling
        /// (Escape / click on empty space) can restore focus to whatever
        /// the user was actually doing. The bar itself never becomes
        /// foreground (`WS_EX_NOACTIVATE`), so this is never just the bar.
        previous_foreground: HWND,
    }

    thread_local! {
        static STATE: RefCell<Option<AppState>> = const { RefCell::new(None) };
    }

    pub fn main() -> Result<()> {
        let _log_guard = groveshell_common::logging::init("ui")?;
        tracing::info!("groveshell-ui starting");

        let _job = groveshell_common::jobobject::ShellJob::create_and_join()?;
        tracing::info!("joined shell job object");

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

            let screen_w = GetSystemMetrics(SM_CXSCREEN);
            let screen_h = GetSystemMetrics(SM_CYSCREEN);

            // Bar: WS_EX_TOOLWINDOW keeps it out of the taskbar/alt-tab and
            // (as a side effect) out of its own Activities listing, since
            // `window-model::snapshot` excludes tool windows. WS_EX_NOACTIVATE
            // means clicking the Activities label never makes the bar the
            // foreground window — without it, `GetForegroundWindow()` in
            // `open_overview` would see the bar itself instead of whatever
            // app the user was actually using, breaking "restore focus on
            // cancel."
            let bar_hwnd = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                w!("GroveShellBar"),
                w!("GroveShell"),
                WS_POPUP | WS_VISIBLE,
                0,
                0,
                screen_w,
                BAR_HEIGHT,
                None,
                None,
                hinstance,
                None,
            )
            .map_err(Error::Windows)?;

            // Register the bar as a top-edge AppBar (the same mechanism the
            // Windows taskbar uses) so it reserves its strip of the work
            // area instead of just floating on top of maximized windows and
            // desktop icons.
            let bar_rect = register_appbar(bar_hwnd, screen_w, BAR_HEIGHT);
            let _ = MoveWindow(
                bar_hwnd,
                bar_rect.left,
                bar_rect.top,
                bar_rect.right - bar_rect.left,
                bar_rect.bottom - bar_rect.top,
                true,
            );

            // Overview: covers everything below the bar rather than the
            // whole screen, so the bar (and its Activities label) stays put
            // and visible while the overview is open, matching GNOME, where
            // the top bar is never covered by the overview it opens.
            let overview_y = bar_rect.bottom;
            let overview_h = screen_h - overview_y;
            let overview_hwnd = CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW,
                w!("GroveShellOverview"),
                w!("GroveShell Activities"),
                WS_POPUP,
                0,
                overview_y,
                screen_w,
                overview_h,
                None,
                None,
                hinstance,
                None,
            )
            .map_err(Error::Windows)?;

            STATE.with(|s| {
                *s.borrow_mut() = Some(AppState {
                    bar_hwnd,
                    overview_hwnd,
                    overview: OverviewMode::Closed,
                    previous_foreground: HWND(std::ptr::null_mut()),
                });
            });

            // The `MoveWindow` above may have already triggered and
            // consumed a `WM_PAINT` for the bar before `STATE` existed, in
            // which case `wndproc` fell back to `DefWindowProcW` and the
            // Activities label never actually got drawn. Force one more
            // repaint now that `STATE` is ready so the label always shows
            // up, regardless of how that first paint landed.
            let _ = InvalidateRect(bar_hwnd, None, true);

            let mut msg = MSG::default();
            while GetMessageW(&mut msg, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }

        Ok(())
    }

    /// Registers `bar_hwnd` as a top-edge AppBar and reserves a
    /// `bar_height`-tall strip of the primary monitor for it, returning the
    /// rect the system assigned (per `ABM_SETPOS` semantics, this is what
    /// the caller should actually move/resize the window to). Every other
    /// top-level window's maximize/work-area layout is recalculated by the
    /// system as a side effect, exactly as it is for the real taskbar.
    ///
    /// SAFETY: `bar_hwnd` must be a live window for the duration of this
    /// call; `SHAppBarMessage` only reads/writes through the `APPBARDATA`
    /// pointer for the duration of each call.
    unsafe fn register_appbar(bar_hwnd: HWND, screen_w: i32, bar_height: i32) -> RECT {
        let mut abd = APPBARDATA {
            cbSize: std::mem::size_of::<APPBARDATA>() as u32,
            hWnd: bar_hwnd,
            ..Default::default()
        };
        SHAppBarMessage(ABM_NEW, &mut abd);

        abd.uEdge = ABE_TOP;
        abd.rc = RECT {
            left: 0,
            top: 0,
            right: screen_w,
            bottom: bar_height,
        };
        // ABM_QUERYPOS lets other appbars adjust the proposed rect (e.g. if
        // the Windows taskbar already sits at the top); our height is
        // fixed regardless, so only `bottom` is reasserted afterward.
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

    /// Paints the "Activities" label directly onto the bar. There's no
    /// native `BUTTON` control here — at a 16px bar height a real push
    /// button's chrome leaves no room for legible text, so this is flat
    /// painted text hit-tested in `WM_LBUTTONUP` instead.
    fn paint_bar(hwnd: HWND) {
        // SAFETY: `hwnd` is the window currently processing `WM_PAINT`, so
        // it's guaranteed valid for the duration of this call; `ps` is a
        // local that outlives the paired `BeginPaint`/`EndPaint` call.
        unsafe {
            let mut ps = PAINTSTRUCT::default();
            let hdc = BeginPaint(hwnd, &mut ps);
            SetBkMode(hdc, TRANSPARENT);
            SetTextColor(hdc, COLORREF(0x00E0E0E0));
            let mut text: Vec<u16> = "Activities".encode_utf16().collect();
            let mut rect = RECT {
                left: ACTIVITIES_LABEL_X,
                top: 0,
                right: ACTIVITIES_LABEL_X + ACTIVITIES_LABEL_WIDTH,
                bottom: BAR_HEIGHT,
            };
            DrawTextW(
                hdc,
                &mut text,
                &mut rect,
                DT_SINGLELINE | DT_VCENTER | DT_CENTER,
            );
            let _ = EndPaint(hwnd, &ps);
        }
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
    /// out toward the center of the overview" transform applied uniformly
    /// to every window's rect, as a group, rather than rearranging them
    /// into a grid.
    fn scale_about(r: RECT, pivot_x: i32, pivot_y: i32, s: f64) -> RECT {
        let scale = |v: i32, pivot: i32| pivot + ((v - pivot) as f64 * s).round() as i32;
        RECT {
            left: scale(r.left, pivot_x),
            top: scale(r.top, pivot_y),
            right: scale(r.right, pivot_x),
            bottom: scale(r.bottom, pivot_y),
        }
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

    /// Starts the "zoom out" animation: registers a live DWM thumbnail for
    /// every currently-eligible window at its real screen position, then
    /// kicks off a timer to animate each one down to `OVERVIEW_SCALE` of
    /// its size, centered in the overview. No-op if the overview isn't
    /// currently `Closed` (already open or mid-animation).
    fn open_overview() {
        let overview_hwnd = STATE.with(|s| {
            s.borrow().as_ref().and_then(|st| {
                matches!(st.overview, OverviewMode::Closed).then_some(st.overview_hwnd)
            })
        });
        let Some(overview_hwnd) = overview_hwnd else {
            return;
        };

        // SAFETY: `overview_hwnd` is a valid, process-lifetime window.
        let mut overview_screen_rect = RECT::default();
        unsafe {
            let _ = GetWindowRect(overview_hwnd, &mut overview_screen_rect);
        }
        let origin_x = overview_screen_rect.left;
        let origin_y = overview_screen_rect.top;
        let pivot_x = (overview_screen_rect.right - origin_x) / 2;
        let pivot_y = (overview_screen_rect.bottom - origin_y) / 2;

        let mut thumbs = Vec::new();
        for window in groveshell_window_model::snapshot() {
            let source = HWND(window.hwnd as *mut c_void);
            // SAFETY: `overview_hwnd` and `source` are both live windows;
            // `source` was enumerated moments ago and may have closed
            // since, in which case this simply fails and is skipped.
            let thumbnail = match unsafe { DwmRegisterThumbnail(overview_hwnd, source) } {
                Ok(handle) => handle,
                Err(_) => continue,
            };

            let from = RECT {
                left: window.rect.left - origin_x,
                top: window.rect.top - origin_y,
                right: window.rect.right - origin_x,
                bottom: window.rect.bottom - origin_y,
            };
            let to = scale_about(from, pivot_x, pivot_y, OVERVIEW_SCALE);

            // Show it immediately at "from" — exactly where the real
            // window currently sits — so the animation reads as that
            // window shrinking away, not popping in from nowhere.
            set_thumb_rect(thumbnail, from);

            thumbs.push(ThumbAnim {
                thumbnail,
                hwnd: source,
                from,
                to,
                current: from,
            });
        }

        // SAFETY: no preconditions.
        let previous_foreground = unsafe { GetForegroundWindow() };

        STATE.with(|s| {
            if let Some(state) = s.borrow_mut().as_mut() {
                state.previous_foreground = previous_foreground;
                state.overview = OverviewMode::Opening {
                    started: Instant::now(),
                    thumbs,
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

    /// Starts the reverse animation from wherever the thumbnails currently
    /// are back to each window's real position, then hides the overview
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
                OverviewMode::Open { thumbs } | OverviewMode::Opening { thumbs, .. } => {
                    let thumbs = thumbs
                        .into_iter()
                        .map(|th| ThumbAnim {
                            from: th.current,
                            to: th.from,
                            current: th.current,
                            ..th
                        })
                        .collect();
                    state.overview = OverviewMode::Closing {
                        started: Instant::now(),
                        thumbs,
                        focus_after,
                    };
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

    fn on_activities_clicked() {
        let is_closed = STATE
            .with(|s| s.borrow().as_ref().map(|st| matches!(st.overview, OverviewMode::Closed)));
        match is_closed {
            Some(true) => open_overview(),
            Some(false) => close_overview(None),
            None => {}
        }
    }

    /// Hit-tests a click against the overview's current thumbnail rects
    /// (only meaningful while `Open`); focuses the clicked window if any,
    /// otherwise treats it as "click on empty space" and just cancels.
    fn on_overview_click(x: i32, y: i32) {
        let hit = STATE.with(|s| {
            s.borrow().as_ref().and_then(|st| match &st.overview {
                OverviewMode::Open { thumbs } => thumbs.iter().find_map(|th| {
                    let r = th.current;
                    (x >= r.left && x < r.right && y >= r.top && y < r.bottom)
                        .then_some(th.hwnd)
                }),
                _ => None,
            })
        });

        close_overview(hit);
    }

    /// Advances whichever animation (`Opening`/`Closing`) is in flight by
    /// one tick, or finalizes it once it reaches the end.
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

            let mode = std::mem::replace(&mut state.overview, OverviewMode::Closed);
            let (new_mode, updates, completion) = match mode {
                OverviewMode::Opening {
                    started,
                    mut thumbs,
                } => {
                    let t = progress(started);
                    let eased = ease_out(t);
                    let updates = tick_thumbs(&mut thumbs, eased);
                    if t >= 1.0 {
                        (OverviewMode::Open { thumbs }, updates, Some(Completion::Opened))
                    } else {
                        (OverviewMode::Opening { started, thumbs }, updates, None)
                    }
                }
                OverviewMode::Closing {
                    started,
                    mut thumbs,
                    focus_after,
                } => {
                    let t = progress(started);
                    let eased = ease_out(t);
                    let updates = tick_thumbs(&mut thumbs, eased);
                    if t >= 1.0 {
                        (
                            OverviewMode::Closed,
                            updates,
                            Some(Completion::Closed { focus_after, thumbs }),
                        )
                    } else {
                        (
                            OverviewMode::Closing {
                                started,
                                thumbs,
                                focus_after,
                            },
                            updates,
                            None,
                        )
                    }
                }
                other => (other, Vec::new(), None),
            };
            state.overview = new_mode;
            Some((state.overview_hwnd, updates, completion))
        });

        let Some((overview_hwnd, updates, completion)) = result else {
            return;
        };

        for (thumbnail, rect) in updates {
            set_thumb_rect(thumbnail, rect);
        }

        let Some(completion) = completion else {
            return;
        };

        // SAFETY: `overview_hwnd` is a valid, process-lifetime window; none
        // of the calls below happen while a `STATE` borrow is held.
        unsafe {
            let _ = KillTimer(overview_hwnd, ANIM_TIMER_ID);
        }

        match completion {
            Completion::Opened => {}
            Completion::Closed { focus_after, thumbs } => {
                for th in &thumbs {
                    // SAFETY: `th.thumbnail` was registered in
                    // `open_overview` and not yet unregistered.
                    unsafe {
                        let _ = DwmUnregisterThumbnail(th.thumbnail);
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

    fn progress(started: Instant) -> f64 {
        (started.elapsed().as_secs_f64() / ANIM_DURATION.as_secs_f64()).min(1.0)
    }

    fn tick_thumbs(thumbs: &mut [ThumbAnim], t: f64) -> Vec<(isize, RECT)> {
        thumbs
            .iter_mut()
            .map(|th| {
                th.current = lerp_rect(th.from, th.to, t);
                (th.thumbnail, th.current)
            })
            .collect()
    }

    unsafe extern "system" fn wndproc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        let is_bar = STATE.with(|s| s.borrow().as_ref().is_some_and(|st| st.bar_hwnd == hwnd));

        match msg {
            WM_PAINT if is_bar => {
                paint_bar(hwnd);
                LRESULT(0)
            }
            WM_LBUTTONUP if is_bar => {
                let x = (lparam.0 & 0xFFFF) as i32;
                if (ACTIVITIES_LABEL_X..ACTIVITIES_LABEL_X + ACTIVITIES_LABEL_WIDTH).contains(&x) {
                    on_activities_clicked();
                }
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                let x = (lparam.0 & 0xFFFF) as i32;
                let y = ((lparam.0 >> 16) & 0xFFFF) as i32;
                on_overview_click(x, y);
                LRESULT(0)
            }
            WM_KEYDOWN if !is_bar => {
                if wparam.0 == VK_ESCAPE.0 as usize {
                    close_overview(None);
                }
                LRESULT(0)
            }
            WM_TIMER => {
                on_animation_tick();
                LRESULT(0)
            }
            WM_DESTROY => {
                if is_bar {
                    unregister_appbar(hwnd);
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
