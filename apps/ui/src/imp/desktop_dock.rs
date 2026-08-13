//! The opt-in persistent desktop dock (spec §5.3 / §7). Off by default
//! (`dock_mode = "overview"`); `"always"` shows a floating dock at the
//! bottom of the primary monitor, `"autohide"` slides it away and reveals
//! it when the pointer hits the bottom edge.
//!
//! Deliberately a **floating, top-most overlay** — it does *not* register
//! an AppBar or reserve work area. Reserving a strip is what can strand the
//! desktop when a window mis-registers (the project has fought exactly that
//! with the taskbar), and a persistent dock doesn't need it: it simply
//! floats over the bottom of the screen like a GNOME dash. This keeps the
//! risky work-area machinery out of an opt-in surface entirely.
//!
//! This module owns the reveal state machine and geometry (pure, tested);
//! the window creation/paint wiring lives alongside it and reuses the
//! overview dock's app model (`dock::build_dock_apps`) rather than
//! duplicating icon logic.

use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
use std::time::{Duration, Instant};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{HWND, POINT, RECT};
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::Direct2D::ID2D1DeviceContext;
use windows::Win32::Graphics::Gdi::DeleteObject;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    AppendMenuW, CreatePopupMenu, CreateWindowExW, DestroyMenu, GetCursorPos, IsIconic, KillTimer,
    SetForegroundWindow, SetTimer, SetWindowPos, ShowWindow, TrackPopupMenu, HWND_TOPMOST,
    MF_STRING, SWP_NOACTIVATE, SW_RESTORE, SW_SHOWNA, SW_SHOWNORMAL, TPM_LEFTALIGN, TPM_RETURNCMD,
    TPM_TOPALIGN, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use super::design::motion::{self, BASE_MS};
use super::dock::DockApp;
use super::gpu::{self, GpuSurface};

/// Timer id for the desktop dock's own animation/autohide tick (distinct
/// from the overview's `ANIM_TIMER_ID` and the others in `movesize`/
/// `workspaces`).
pub(crate) const DOCK_TIMER_ID: usize = 6;
const DOCK_TIMER_INTERVAL_MS: u32 = 16;
/// How close to the screen's bottom edge the pointer must get to reveal an
/// autohidden dock (96-DPI band).
const REVEAL_BAND: i32 = 3;

/// Right-click menu command ids.
const MENU_PIN: u32 = 1;
const MENU_OPEN_NEW: u32 = 2;

/// 96-DPI layout metrics for the floating desktop dock. Deliberately a
/// touch roomier than the overview dock (`dock.rs`) so it reads as a
/// first-class, macOS-style dock rather than a compact dash.
pub(crate) const ICON_GAP: i32 = 12;
pub(crate) const PAD_X: i32 = 14;
pub(crate) const PAD_Y: i32 = 10;
pub(crate) const CORNER_RADIUS: i32 = 22;
/// Gap between the dock panel's bottom and the monitor's work-area bottom.
pub(crate) const MARGIN_BOTTOM: i32 = 10;
/// Peak magnification of the icon directly under the pointer — the crest
/// of the wave. 1.0 would be no magnification.
pub(crate) const MAX_SCALE: f32 = 1.65;
/// Running-indicator dot radius (96-DPI).
pub(crate) const RUNNING_DOT_RADIUS: i32 = 3;

/// The dock's fixed (resting) panel geometry in the dock window's own
/// local coordinates, plus each icon's baseline center-x. The window is
/// sized taller and a little wider than the panel so magnified icons can
/// grow upward out of the panel and outward at the ends without being
/// clipped — the classic dock "the icon pops above the tray" look.
///
/// Returns `(panel_rect, icon_centers, baseline_bottom, window_w,
/// window_h)`, all DPI-scaled. `baseline_bottom` is the local y that every
/// icon's *bottom* edge sits on; icons grow upward from it as they
/// magnify. Pure and total (clamps an empty dock to one slot) so it's unit
/// testable without a live monitor.
pub(crate) fn panel_geometry(app_count: usize, icon_size_ref: i32, dpi: u32) -> PanelGeometry {
    let scaled = |v: i32| super::state::scaled(v, dpi);
    let icon = super::state::scaled(icon_size_ref, dpi).max(1);
    let gap = scaled(ICON_GAP);
    let pad_x = scaled(PAD_X);
    let pad_y = scaled(PAD_Y);

    let count = app_count.max(1) as i32;
    let panel_w = count * icon + (count - 1).max(0) * gap + pad_x * 2;
    let panel_h = icon + pad_y * 2;

    let max_icon = (icon as f32 * MAX_SCALE).round() as i32;
    // Side headroom so an end icon at full magnification (scaled about its
    // own center) doesn't clip the window edge; top headroom so a crest
    // icon grows fully above the panel.
    let side_pad = (max_icon - icon) / 2 + gap;
    let top_pad = (max_icon - icon).max(0) + pad_y;
    let window_w = panel_w + side_pad * 2;
    let window_h = panel_h + top_pad;

    let panel_left = side_pad;
    let panel_top = window_h - panel_h;
    let panel_rect = RECT {
        left: panel_left,
        top: panel_top,
        right: panel_left + panel_w,
        bottom: window_h,
    };
    let baseline_bottom = window_h - pad_y;

    let mut centers = Vec::with_capacity(count as usize);
    let first_center = panel_left + pad_x + icon / 2;
    for i in 0..count {
        centers.push(first_center + i * (icon + gap));
    }

    PanelGeometry { panel_rect, centers, baseline_bottom, window_w, window_h, icon }
}

/// Result of [`panel_geometry`] — see its doc comment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PanelGeometry {
    pub(crate) panel_rect: RECT,
    pub(crate) centers: Vec<i32>,
    pub(crate) baseline_bottom: i32,
    pub(crate) window_w: i32,
    pub(crate) window_h: i32,
    /// The resting (un-magnified) icon size, DPI-scaled.
    pub(crate) icon: i32,
}

/// A smooth wave-magnification factor (>= 1.0) for an icon whose baseline
/// center is `distance` px from the pointer, peaking at [`MAX_SCALE`]
/// directly under it and easing back to 1.0 with a Gaussian falloff whose
/// width is `sigma`. `cursor` of `None` (pointer not over the dock) yields
/// 1.0 everywhere — a flat, resting dock. Pure and cheap.
pub(crate) fn magnify_factor(distance: f32, sigma: f32) -> f32 {
    let sigma = sigma.max(1.0);
    let x = distance / sigma;
    1.0 + (MAX_SCALE - 1.0) * (-0.5 * x * x).exp()
}

/// Each icon's on-screen rect for the current pointer position, applying
/// the wave: every icon stays centered on its fixed baseline center and
/// bottom-anchored to `baseline_bottom`, scaled by [`magnify_factor`] of
/// its distance to `cursor_x` (window-local). `progress` (0..1) eases the
/// whole effect in/out so the wave doesn't snap on when the pointer
/// arrives — the rendered scale is lerped from 1.0 toward its target by
/// `progress`. Fixed centers keep hit-testing stable (see
/// [`icon_at`]). Pure.
pub(crate) fn wave_icon_rects(
    geo: &PanelGeometry,
    cursor_x: Option<i32>,
    progress: f32,
) -> Vec<RECT> {
    let progress = progress.clamp(0.0, 1.0);
    let sigma = (geo.icon as f32) * 1.3;
    geo.centers
        .iter()
        .map(|&cx| {
            let target = match cursor_x {
                Some(x) => magnify_factor((x - cx).abs() as f32, sigma),
                None => 1.0,
            };
            let scale = 1.0 + (target - 1.0) * progress;
            let size = (geo.icon as f32 * scale).round() as i32;
            let half = size / 2;
            RECT {
                left: cx - half,
                top: geo.baseline_bottom - size,
                right: cx - half + size,
                bottom: geo.baseline_bottom,
            }
        })
        .collect()
}

/// Which icon index the pointer at window-local `x` is over, or `None` if
/// it's past the ends. Hit-tests the *fixed* baseline centers (pitch =
/// icon + gap), so a click lands on the same app whether the dock is
/// resting or mid-wave — the magnified rects only move things visually.
pub(crate) fn icon_at(geo: &PanelGeometry, x: i32, dpi: u32) -> Option<usize> {
    if geo.centers.is_empty() {
        return None;
    }
    let pitch = geo.icon + super::state::scaled(ICON_GAP, dpi);
    let half = pitch / 2;
    geo.centers
        .iter()
        .enumerate()
        .find(|(_, &cx)| (x - cx).abs() <= half)
        .map(|(i, _)| i)
}

/// Autohide reveal phases. `Shown`/`Hidden` are the resting states;
/// `Revealing`/`Hiding` are the sliding transitions.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum RevealPhase {
    Hidden,
    Revealing,
    Shown,
    Hiding,
}

/// The autohide reveal state machine. Driven by pointer hot-edge hits and
/// leaves, advanced by `tick`.
pub(crate) struct AutoHide {
    pub(crate) phase: RevealPhase,
    started: Instant,
    duration: Duration,
}

impl AutoHide {
    pub(crate) fn new() -> Self {
        Self { phase: RevealPhase::Hidden, started: Instant::now(), duration: Duration::ZERO }
    }

    /// The pointer reached the bottom hot-edge: start revealing (or cancel
    /// an in-progress hide, snapping back to fully shown).
    pub(crate) fn on_hot_edge(&mut self) {
        match self.phase {
            RevealPhase::Hidden => self.begin(RevealPhase::Revealing),
            RevealPhase::Hiding => self.phase = RevealPhase::Shown,
            _ => {}
        }
    }

    /// The pointer left the dock (after a dwell): start hiding.
    pub(crate) fn on_leave(&mut self) {
        if matches!(self.phase, RevealPhase::Shown | RevealPhase::Revealing) {
            self.begin(RevealPhase::Hiding);
        }
    }

    fn begin(&mut self, transient: RevealPhase) {
        let ms = motion::effective_ms(BASE_MS);
        self.started = Instant::now();
        self.duration = Duration::from_millis(ms as u64);
        if ms == 0 {
            self.phase = terminal_of(transient);
        } else {
            self.phase = transient;
        }
    }

    /// Advances to `now`, completing `Revealing→Shown` / `Hiding→Hidden`,
    /// and returns the eased 0..1 progress of the current transition (1.0
    /// when resting).
    pub(crate) fn tick(&mut self, now: Instant) -> f32 {
        match self.phase {
            RevealPhase::Shown | RevealPhase::Hidden => 1.0,
            RevealPhase::Revealing | RevealPhase::Hiding => {
                let raw = if self.duration.is_zero() {
                    1.0
                } else {
                    (now.saturating_duration_since(self.started).as_secs_f32()
                        / self.duration.as_secs_f32())
                    .clamp(0.0, 1.0)
                };
                if raw >= 1.0 {
                    self.phase = terminal_of(self.phase);
                    return 1.0;
                }
                motion::ease_out_cubic(raw)
            }
        }
    }

    pub(crate) fn is_animating(&self) -> bool {
        matches!(self.phase, RevealPhase::Revealing | RevealPhase::Hiding)
    }

    /// Vertical offset (px) to draw the dock at for the current phase and
    /// tick progress `p`: `0` fully shown, `height` fully hidden below the
    /// screen edge. Revealing slides up (`height→0`), Hiding slides down.
    pub(crate) fn offset(&self, p: f32, height: i32) -> i32 {
        let h = height as f32;
        let off = match self.phase {
            RevealPhase::Shown => 0.0,
            RevealPhase::Hidden => h,
            RevealPhase::Revealing => h * (1.0 - p),
            RevealPhase::Hiding => h * p,
        };
        off.round() as i32
    }

    /// Forces the reveal phase directly — used at dock creation to set the
    /// initial resting state (Shown for always, Hidden for autohide) and by
    /// tests.
    pub(crate) fn force(&mut self, phase: RevealPhase) {
        self.phase = phase;
    }
}

fn terminal_of(transient: RevealPhase) -> RevealPhase {
    match transient {
        RevealPhase::Revealing => RevealPhase::Shown,
        RevealPhase::Hiding => RevealPhase::Hidden,
        other => other,
    }
}

/// The live floating desktop dock: its window, GPU surface, the reveal
/// state machine, the current app model, and the transient interaction
/// state (pointer position, hovered icon, eased wave progress). One of
/// these exists in `AppState.dock` whenever `dock_mode` is `"always"` or
/// `"autohide"`; `"overview"` leaves it `None`.
pub(crate) struct DockWindow {
    pub(crate) hwnd: HWND,
    gpu: Option<GpuSurface>,
    autohide: AutoHide,
    apps: Vec<DockApp>,
    /// The primary monitor's work area (screen coords) and DPI the dock
    /// is laid out against.
    work: RECT,
    dpi: u32,
    /// Configured resting icon size (96-DPI reference), from
    /// `dock_icon_size`.
    icon_size_ref: i32,
    /// `"always"` or `"autohide"`.
    autohidden: bool,
    geo: PanelGeometry,
    /// Resting top-left of the window on screen (fully shown).
    base_left: i32,
    base_top: i32,
    /// Pointer x in window-local coords while it's over the dock, else
    /// `None` (a flat, resting dock).
    cursor_x: Option<i32>,
    /// Eased 0..1 magnification progress — the wave fades in when the
    /// pointer arrives and out when it leaves, rather than snapping.
    wave: f32,
    /// Last window rect we positioned to, so `tick` only calls
    /// `SetWindowPos` when it actually moved.
    last_offset: i32,
    /// Frame counter, driving the slow app-refresh cadence.
    ticks: u64,
    /// Cheap signature of the last-seen running-window set + pinned list,
    /// so the periodic refresh skips the expensive `build_dock_apps` (COM
    /// shortcut/icon resolution) when nothing actually changed.
    last_sig: u64,
}

/// A cheap order-independent-enough fold of the running windows and pins,
/// used to detect whether the dock's contents changed since last refresh.
fn contents_signature(live: &[groveshell_window_model::WindowRecord], pin_count: usize) -> u64 {
    let mut sig = 1469598103934665603u64 ^ (live.len() as u64).wrapping_shl(1);
    for w in live {
        sig = (sig ^ (w.hwnd as u64)).wrapping_mul(1099511628211);
    }
    sig ^ (pin_count as u64).wrapping_shl(48)
}

impl DockWindow {
    /// Creates a floating dock window on the monitor whose `work` area is
    /// given, for the given mode (`"always"` or `"autohide"` — any other
    /// value is treated as always-shown). `None` if window creation fails,
    /// in which case the shell simply runs without a persistent dock,
    /// unchanged.
    pub(crate) fn create(
        hinstance: windows::Win32::Foundation::HINSTANCE,
        work: RECT,
        dpi: u32,
        icon_size_ref: i32,
        mode: &str,
    ) -> Option<DockWindow> {
        let live = super::state::filter_ignored(groveshell_window_model::snapshot());
        let sig = contents_signature(&live, super::dock::pinned_paths().len());
        let apps = super::dock::build_dock_apps(&live);
        let geo = panel_geometry(apps.len(), icon_size_ref, dpi);
        let base_left = work.left + (work.right - work.left - geo.window_w) / 2;
        let base_top = work.bottom - super::state::scaled(MARGIN_BOTTOM, dpi) - geo.window_h;

        // SAFETY: standard top-most tool-window creation; `hinstance` is the
        // process module handle. `WS_EX_NOACTIVATE` keeps clicks from
        // stealing focus from the app the user is working in.
        let hwnd = unsafe {
            CreateWindowExW(
                WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
                w!("GroveShellDock"),
                w!("GroveShell Dock"),
                WS_POPUP,
                base_left,
                base_top,
                geo.window_w,
                geo.window_h,
                None,
                None,
                hinstance,
                None,
            )
            .ok()?
        };

        let gpu = gpu::create_surface(hwnd, geo.window_w, geo.window_h);

        let mut autohide = AutoHide::new();
        let autohidden = mode == "autohide";
        autohide.force(if autohidden { RevealPhase::Hidden } else { RevealPhase::Shown });

        let mut dock = DockWindow {
            hwnd,
            gpu,
            autohide,
            apps,
            work,
            dpi,
            icon_size_ref,
            autohidden,
            geo,
            base_left,
            base_top,
            cursor_x: None,
            wave: 0.0,
            last_offset: -1,
            ticks: 0,
            last_sig: sig,
        };
        dock.reposition(true);
        dock.repaint();

        // SAFETY: `hwnd` was just created; `SW_SHOWNA` shows without
        // activating (matching `WS_EX_NOACTIVATE`).
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOWNA);
            SetTimer(hwnd, DOCK_TIMER_ID, DOCK_TIMER_INTERVAL_MS, None);
        }
        Some(dock)
    }

    /// Rebuilds the app model and geometry when the running-window set,
    /// pins, or configured icon size actually changed since last time —
    /// short-circuiting the expensive `build_dock_apps` (COM shortcut/icon
    /// resolution) otherwise, so the timer stays cheap. Recreates the GPU
    /// surface if the window resized (a different app count). Returns
    /// whether anything changed (so the caller repaints/repositions).
    fn refresh(&mut self, icon_size_ref: i32) -> bool {
        let live = super::state::filter_ignored(groveshell_window_model::snapshot());
        let sig = contents_signature(&live, super::dock::pinned_paths().len());
        if icon_size_ref == self.icon_size_ref && sig == self.last_sig {
            return false;
        }
        self.last_sig = sig;
        self.icon_size_ref = icon_size_ref;
        self.apps = super::dock::build_dock_apps(&live);

        let old = (self.geo.window_w, self.geo.window_h);
        self.geo = panel_geometry(self.apps.len(), self.icon_size_ref, self.dpi);
        self.base_left = self.work.left + (self.work.right - self.work.left - self.geo.window_w) / 2;
        self.base_top =
            self.work.bottom - super::state::scaled(MARGIN_BOTTOM, self.dpi) - self.geo.window_h;
        // A resized window needs a surface at the new size, or the panel
        // would be clipped/misplaced against a stale-sized surface.
        if old != (self.geo.window_w, self.geo.window_h) {
            // Drop the old surface/target first: a window can hold only one
            // DirectComposition target, so creating the new one while the
            // old still lives would fail.
            self.gpu = None;
            self.gpu = gpu::create_surface(self.hwnd, self.geo.window_w, self.geo.window_h);
        }
        // Force a reposition next tick (window moved/resized).
        self.last_offset = -1;
        true
    }

    /// Moves/resizes the window for the current autohide slide offset.
    /// `force` bypasses the "only if it moved" guard (used at creation and
    /// after a geometry change).
    fn reposition(&mut self, force: bool) {
        let p = 1.0; // resting; `tick` passes eased progress via `slide_offset`
        let offset = self.autohide.offset(p, self.geo.window_h);
        self.apply_offset(offset, force);
    }

    fn apply_offset(&mut self, offset: i32, force: bool) {
        if !force && offset == self.last_offset {
            return;
        }
        self.last_offset = offset;
        // SAFETY: `self.hwnd` is the live dock window; `SetWindowPos` with
        // `HWND_TOPMOST` keeps it above ordinary windows without activating.
        unsafe {
            let _ = SetWindowPos(
                self.hwnd,
                HWND_TOPMOST,
                self.base_left,
                self.base_top + offset,
                self.geo.window_w,
                self.geo.window_h,
                SWP_NOACTIVATE,
            );
        }
    }

    /// Repaints the dock surface: the rounded panel, each icon at its
    /// current wave size, and running-indicator dots.
    fn repaint(&self) {
        let Some(surface) = self.gpu.as_ref() else { return };
        let rects = wave_icon_rects(&self.geo, self.cursor_x, self.wave);
        let panel = &self.geo.panel_rect;
        let radius = super::state::scaled(CORNER_RADIUS, self.dpi) as f32;
        let dot_radius = super::state::scaled(RUNNING_DOT_RADIUS, self.dpi);
        let apps = &self.apps;

        gpu::redraw(surface, |ctx: &ID2D1DeviceContext| {
            // The window is taller than the panel (headroom for magnified
            // icons); everything above the panel must stay transparent so
            // the dock reads as a floating bar, not a dark box.
            gpu::clear_transparent(ctx);
            let panel_rect = to_d2d(*panel);
            gpu::fill_rounded_rect(ctx, panel_rect, radius, super::design::color::surface_raised());
            gpu::stroke_rounded_rect(ctx, panel_rect, radius, super::design::color::stroke(), 0.6, 1.0);

            // No hover plate behind the icon: the magnification itself is
            // the hover feedback (macOS-style). A plate drawn at the
            // magnified icon's rect rose *above* the panel with the icon,
            // reading as a gray box jumping up behind it and breaking the
            // "only the icon moves, the platform stays put" feel.
            for (app, slot) in apps.iter().zip(rects.iter()) {
                if let Some(icon) = app.icon {
                    let size = (slot.right - slot.left).max(1);
                    if let Some(hbitmap) = super::overview_gpu::icon_to_hbitmap(icon, size) {
                        if let Some(bitmap) = gpu::bitmap_from_hbitmap(ctx, hbitmap) {
                            gpu::draw_bitmap_stretched(ctx, to_d2d(*slot), &bitmap);
                        }
                        // SAFETY: created locally above, owned exclusively here.
                        unsafe {
                            let _ = DeleteObject(hbitmap);
                        }
                    }
                }
                // Running indicator: one dot centered under the icon, in
                // the panel's bottom padding.
                if !app.windows.is_empty() {
                    let cx = (slot.left + slot.right) / 2;
                    let cy = self.geo.baseline_bottom + (self.geo.window_h - self.geo.baseline_bottom) / 2;
                    let dot = RECT {
                        left: cx - dot_radius,
                        top: cy - dot_radius,
                        right: cx + dot_radius,
                        bottom: cy + dot_radius,
                    };
                    gpu::fill_rounded_rect(ctx, to_d2d(dot), dot_radius as f32, super::design::color::accent());
                }
            }
        });
    }

    /// Pointer moved to window-local `(x, y)`: track it for the wave.
    pub(crate) fn on_mouse_move(&mut self, x: i32, _y: i32) {
        self.cursor_x = Some(x);
    }

    /// A click at window-local `x`: focus (or launch) the icon under it.
    pub(crate) fn on_click(&mut self, x: i32) {
        if let Some(i) = icon_at(&self.geo, x, self.dpi) {
            if let Some(app) = self.apps.get(i) {
                focus_or_launch(app);
            }
        }
    }

    /// A right-click at window-local `x`: pin/unpin (and "open new window")
    /// for the icon under it, via the same persisted pinned list the
    /// overview dash uses — so the two surfaces stay in sync. A running-
    /// but-unpinned entry (no launch path) gets no menu, matching the
    /// overview dock.
    pub(crate) fn on_right_click(&mut self, x: i32) {
        let Some(i) = icon_at(&self.geo, x, self.dpi) else { return };
        let Some(launch_path) = self.apps.get(i).and_then(|a| a.launch_path.clone()) else {
            return;
        };

        // SAFETY: a standard, synchronous popup-menu sequence; `menu` is
        // destroyed on every path before returning.
        unsafe {
            let Ok(menu) = CreatePopupMenu() else { return };
            let is_pinned = super::dock::pinned_paths().contains(&launch_path);
            let pin_label = if is_pinned { w!("Unpin from dock") } else { w!("Pin to dock") };
            let _ = AppendMenuW(menu, MF_STRING, MENU_PIN as usize, pin_label);
            let _ = AppendMenuW(menu, MF_STRING, MENU_OPEN_NEW as usize, w!("Open new window"));

            let mut point = POINT::default();
            let _ = GetCursorPos(&mut point);
            // A top-most `WS_EX_NOACTIVATE` window must be foregrounded for
            // `TrackPopupMenu` to stay open — same dance the overview dock
            // does for its menu.
            let _ = SetForegroundWindow(self.hwnd);
            let cmd = TrackPopupMenu(
                menu,
                TPM_RETURNCMD | TPM_LEFTALIGN | TPM_TOPALIGN,
                point.x,
                point.y,
                0,
                self.hwnd,
                None,
            );
            let _ = DestroyMenu(menu);

            match cmd.0 as u32 {
                MENU_PIN => {
                    if is_pinned {
                        super::dock::unpin_app(&launch_path);
                    } else {
                        super::dock::pin_app(launch_path);
                    }
                }
                MENU_OPEN_NEW => {
                    let wide: Vec<u16> =
                        launch_path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
                    let _ = ShellExecuteW(
                        HWND(std::ptr::null_mut()),
                        w!("open"),
                        PCWSTR(wide.as_ptr()),
                        PCWSTR::null(),
                        PCWSTR::null(),
                        SW_SHOWNORMAL,
                    );
                }
                _ => {}
            }
        }

        // Reflect a pin/unpin immediately (the slow refresh would catch it
        // within ~1s anyway, but this feels instant).
        let (icon_size, _) = super::state::dock_config();
        if self.refresh(icon_size) {
            self.reposition(true);
        }
        self.repaint();
    }

    /// One animation frame: advance the autohide slide, ease the wave
    /// toward its target (magnified while hovered, flat otherwise), refresh
    /// the app model periodically, and repaint/reposition only when
    /// something actually changed.
    pub(crate) fn tick(&mut self) {
        self.ticks = self.ticks.wrapping_add(1);
        let tick_count = self.ticks;
        let now = Instant::now();

        // Autohide reveal/hide driven by the global pointer position.
        if self.autohidden {
            self.update_autohide();
        } else {
            // Always-shown mode gets no `WM_MOUSELEAVE` plumbing; poll the
            // global pointer so the wave recedes once it leaves the window.
            let mut pt = POINT::default();
            // SAFETY: `GetCursorPos` writes a plain out-param.
            if unsafe { GetCursorPos(&mut pt).is_ok() } {
                let over = pt.x >= self.base_left
                    && pt.x < self.base_left + self.geo.window_w
                    && pt.y >= self.base_top
                    && pt.y < self.base_top + self.geo.window_h;
                if !over {
                    self.cursor_x = None;
                }
            }
        }
        let slide_p = self.autohide.tick(now);
        let offset = self.autohide.offset(slide_p, self.geo.window_h);

        // Ease the wave toward its target. When hidden/hiding, force it flat.
        let shown = matches!(self.autohide.phase, RevealPhase::Shown | RevealPhase::Revealing);
        let target = if shown && self.cursor_x.is_some() { 1.0 } else { 0.0 };
        let prev_wave = self.wave;
        // ~120ms ease at 16ms/frame: step ~0.13 toward target.
        let step = 0.16_f32;
        if (self.wave - target).abs() <= step {
            self.wave = target;
        } else if self.wave < target {
            self.wave += step;
        } else {
            self.wave -= step;
        }

        // Periodically pick up newly-opened/closed apps and pin changes
        // (every ~1s). `refresh` is a cheap no-op when nothing changed.
        if tick_count % 64 == 0 {
            let (icon_size, _) = super::state::dock_config();
            if self.refresh(icon_size) {
                self.reposition(true);
                self.repaint();
            }
        }

        let moved = offset != self.last_offset;
        if moved {
            self.apply_offset(offset, false);
        }
        // Repaint if the wave progress changed, we're mid-slide, or the
        // pointer is currently over the dock (so the crest follows it
        // horizontally even when the overall progress is steady at 1.0).
        let wave_changed = (self.wave - prev_wave).abs() > f32::EPSILON;
        if wave_changed || moved || self.autohide.is_animating() || self.cursor_x.is_some() {
            self.repaint();
        }
    }

    /// The panel's top edge in window-local coordinates — the boundary
    /// `wndproc` uses to make the empty headroom above the panel
    /// click-through (see the dock's `WM_NCHITTEST` handling).
    pub(crate) fn panel_top_local(&self) -> i32 {
        self.geo.panel_rect.top
    }

    /// Decides reveal vs. hide for autohide mode from the live cursor
    /// position: reveal when the pointer hits the bottom edge under the
    /// dock's footprint; hide once it leaves that footprint.
    fn update_autohide(&mut self) {
        let mut pt = POINT::default();
        // SAFETY: `GetCursorPos` writes a plain out-param; no preconditions.
        let ok = unsafe { GetCursorPos(&mut pt).is_ok() };
        if !ok {
            return;
        }
        let in_x = pt.x >= self.base_left && pt.x < self.base_left + self.geo.window_w;
        let over_footprint = in_x && pt.y >= self.base_top;
        let at_edge =
            in_x && pt.y >= self.work.bottom - super::state::scaled(REVEAL_BAND, self.dpi);

        match self.autohide.phase {
            RevealPhase::Hidden | RevealPhase::Hiding => {
                if at_edge {
                    self.autohide.on_hot_edge();
                }
            }
            RevealPhase::Shown | RevealPhase::Revealing => {
                if !over_footprint {
                    self.autohide.on_leave();
                    self.cursor_x = None;
                }
            }
        }
    }

    /// Tears the window down (timer + window). Called when switching to
    /// `"overview"` mode on a live config reload.
    pub(crate) fn destroy(&mut self) {
        // SAFETY: `self.hwnd` is the live dock window; killing its timer and
        // destroying it is a standard teardown.
        unsafe {
            let _ = KillTimer(self.hwnd, DOCK_TIMER_ID);
            let _ = windows::Win32::UI::WindowsAndMessaging::DestroyWindow(self.hwnd);
        }
    }
}

/// Focuses an app's window (restoring it if minimized) or, if it has none,
/// launches its pinned shortcut.
fn focus_or_launch(app: &DockApp) {
    if let Some(&hwnd) = app.windows.first() {
        let h = HWND(hwnd as *mut c_void);
        // SAFETY: `h` is a tracked top-level window handle; restoring and
        // foregrounding it are standard, precondition-free calls.
        unsafe {
            if IsIconic(h).as_bool() {
                let _ = ShowWindow(h, SW_RESTORE);
            }
            let _ = SetForegroundWindow(h);
        }
    } else if let Some(path) = &app.launch_path {
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
        // SAFETY: `wide` is nul-terminated and outlives the fire-and-forget
        // call — same launch path the overview dock uses.
        unsafe {
            let _ = ShellExecuteW(
                HWND(std::ptr::null_mut()),
                w!("open"),
                PCWSTR(wide.as_ptr()),
                PCWSTR::null(),
                PCWSTR::null(),
                SW_SHOWNORMAL,
            );
        }
    }
}

fn to_d2d(r: RECT) -> D2D_RECT_F {
    D2D_RECT_F {
        left: r.left as f32,
        top: r.top as f32,
        right: r.right as f32,
        bottom: r.bottom as f32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hot_edge_from_hidden_starts_revealing_or_shows() {
        let mut a = AutoHide::new();
        a.on_hot_edge();
        assert!(matches!(a.phase, RevealPhase::Revealing | RevealPhase::Shown));
    }

    #[test]
    fn leave_from_shown_starts_hiding_or_hides() {
        let mut a = AutoHide::new();
        a.force(RevealPhase::Shown);
        a.on_leave();
        assert!(matches!(a.phase, RevealPhase::Hiding | RevealPhase::Hidden));
    }

    #[test]
    fn hot_edge_cancels_an_in_progress_hide() {
        let mut a = AutoHide::new();
        a.force(RevealPhase::Hiding);
        a.on_hot_edge();
        assert_eq!(a.phase, RevealPhase::Shown);
    }

    #[test]
    fn offset_is_zero_when_shown_and_full_when_hidden() {
        let mut a = AutoHide::new();
        a.force(RevealPhase::Shown);
        assert_eq!(a.offset(1.0, 60), 0);
        a.force(RevealPhase::Hidden);
        assert_eq!(a.offset(1.0, 60), 60);
    }

    #[test]
    fn revealing_slides_up_from_hidden_toward_shown() {
        let mut a = AutoHide::new();
        a.force(RevealPhase::Revealing);
        assert_eq!(a.offset(0.0, 60), 60); // just started: fully down
        assert_eq!(a.offset(1.0, 60), 0); // complete: fully up
    }

    #[test]
    fn panel_geometry_spaces_centers_by_one_pitch() {
        let geo = panel_geometry(3, 48, 96);
        assert_eq!(geo.centers.len(), 3);
        let pitch = geo.centers[1] - geo.centers[0];
        assert_eq!(geo.centers[2] - geo.centers[1], pitch);
        // Pitch is one icon plus one gap at this DPI.
        assert_eq!(pitch, 48 + ICON_GAP);
    }

    #[test]
    fn panel_geometry_window_is_taller_and_wider_than_the_panel() {
        let geo = panel_geometry(5, 48, 96);
        let panel_w = geo.panel_rect.right - geo.panel_rect.left;
        let panel_h = geo.panel_rect.bottom - geo.panel_rect.top;
        assert!(geo.window_w > panel_w, "window needs side headroom for end-icon magnification");
        assert!(geo.window_h > panel_h, "window needs top headroom for the wave crest");
    }

    #[test]
    fn panel_geometry_clamps_an_empty_dock_to_one_slot() {
        let geo = panel_geometry(0, 48, 96);
        assert_eq!(geo.centers.len(), 1);
        assert!(geo.window_w > 0 && geo.window_h > 0);
    }

    #[test]
    fn magnify_factor_peaks_under_the_pointer_and_decays() {
        let sigma = 60.0;
        let at_zero = magnify_factor(0.0, sigma);
        assert!((at_zero - MAX_SCALE).abs() < 1e-5);
        assert!(magnify_factor(sigma, sigma) < at_zero);
        assert!(magnify_factor(sigma * 4.0, sigma) < magnify_factor(sigma, sigma));
        // Far away it's essentially resting size.
        assert!(magnify_factor(sigma * 6.0, sigma) < 1.001);
    }

    #[test]
    fn wave_with_no_cursor_leaves_every_icon_at_rest() {
        let geo = panel_geometry(4, 48, 96);
        let rects = wave_icon_rects(&geo, None, 1.0);
        for r in &rects {
            assert_eq!(r.right - r.left, geo.icon);
            assert_eq!(r.bottom, geo.baseline_bottom);
        }
    }

    #[test]
    fn wave_magnifies_the_icon_under_the_cursor_the_most() {
        let geo = panel_geometry(5, 48, 96);
        let cursor = geo.centers[2];
        let rects = wave_icon_rects(&geo, Some(cursor), 1.0);
        let widths: Vec<i32> = rects.iter().map(|r| r.right - r.left).collect();
        assert!(widths[2] > widths[1]);
        assert!(widths[2] > widths[3]);
        // Magnified icons stay bottom-anchored (grow upward).
        for r in &rects {
            assert_eq!(r.bottom, geo.baseline_bottom);
        }
    }

    #[test]
    fn wave_progress_zero_is_a_flat_dock() {
        let geo = panel_geometry(5, 48, 96);
        let rects = wave_icon_rects(&geo, Some(geo.centers[2]), 0.0);
        for r in &rects {
            assert_eq!(r.right - r.left, geo.icon);
        }
    }

    #[test]
    fn icon_at_hits_each_center_and_misses_past_the_ends() {
        let geo = panel_geometry(3, 48, 96);
        assert_eq!(icon_at(&geo, geo.centers[0], 96), Some(0));
        assert_eq!(icon_at(&geo, geo.centers[2], 96), Some(2));
        assert_eq!(icon_at(&geo, geo.centers[0] - 1000, 96), None);
        assert_eq!(icon_at(&geo, geo.centers[2] + 1000, 96), None);
    }
}
