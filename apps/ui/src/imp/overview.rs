//! The Activities overview: the GNOME-style carousel of workspace cards,
//! its open/close zoom animation, dragging (both the carousel itself and
//! individual window previews between cards), type-to-search, and all of
//! the rendering — wallpaper, window-snapshot capture/caching, and the
//! double-buffered paint routine. See the crate-level doc comment for the
//! overall design.

use std::cell::RefCell;
use std::ffi::c_void;
use std::os::windows::ffi::OsStrExt;
use std::time::{Duration, Instant};

use windows::core::{w, PCWSTR};
use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, CreatePen,
    CreateRoundRectRgn, CreateSolidBrush, DeleteDC, DeleteObject, EndPaint, InvalidateRect,
    FillRect, GetDC, GetStockObject, HBITMAP, HDC, ReleaseDC, RoundRect,
    SelectClipRgn, SelectObject, SetBkMode, SetStretchBltMode, SetTextColor, StretchBlt,
    HALFTONE, HOLLOW_BRUSH, NULL_PEN, PAINTSTRUCT, PS_SOLID, SRCCOPY, TRANSPARENT,
    DT_CENTER, DT_END_ELLIPSIS, DT_SINGLELINE, DT_VCENTER,
};
use windows::Win32::Graphics::GdiPlus::{
    GdipCreateBitmapFromFile, GdipCreateFromHDC, GdipDeleteGraphics, GdipDrawImageRectI, GpBitmap,
    GpGraphics, GpImage,
};
use windows::Win32::Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS};
use windows::Win32::UI::Input::KeyboardAndMouse::SetFocus;
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    DrawIconEx, GetClassLongPtrW, GetClientRect, GetForegroundWindow, GetSystemMetrics, IsIconic,
    KillTimer, SendMessageW, SetForegroundWindow, SetLayeredWindowAttributes, SetTimer,
    ShowWindow, DI_NORMAL, GCLP_HICONSM, HICON, ICON_SMALL, LWA_ALPHA,
    SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SPI_GETDESKWALLPAPER,
    SW_HIDE, SW_RESTORE, SW_SHOW, SW_SHOWNORMAL, SystemParametersInfoW, WM_GETICON,
};

use super::bar::{raise_bars_topmost, refresh_bar_indicator};
use super::calendar::hide_calendar;
use super::monitors::monitors_sorted_by_x;
use super::quick_settings::hide_quick_settings;
use super::state::{reference_dpi, scaled, STATE, ANIM_TIMER_ID, ANIM_TIMER_INTERVAL_MS, BAR_HEIGHT};
use super::util::{bar_font, draw_text_in, ease_out, progress, progress_dur};
use super::workspaces::commit_workspace_switch;

/// Below this many pixels of horizontal movement, a press-and-release
/// in the overview is treated as a click (focus/cancel/jump-to-page)
/// rather than a carousel drag.
pub(crate) const CAROUSEL_DRAG_CLICK_THRESHOLD_PX: i32 = 6;
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
/// The overview's zoom-out/zoom-in open/close animation runs between
/// this scale (current card blown up to roughly monitor size —
/// `1 / CARD_WIDTH_FRACTION` makes the card's width match the
/// monitor's) and 1.0 (its normal carousel size).
const OVERVIEW_ZOOM_MAX: f64 = 1.0 / CARD_WIDTH_FRACTION;

/// Overview search: cap on rendered results, and 96-DPI layout metrics
/// for the results panel (see `search_layout`).
const SEARCH_MAX_RESULTS: usize = 8;
const SEARCH_PANEL_WIDTH: i32 = 640;
const SEARCH_ROW_HEIGHT: i32 = 36;
const SEARCH_PANEL_GAP: i32 = 14;

/// One window's grid slot within its workspace's card — a fixed,
/// synthetic layout position (see `layout_grid`), *not* the window's
/// real screen position/size. All rects are `overview_hwnd`-local,
/// **page-local** (pre-carousel-shift) client-area coordinates — the
/// horizontal carousel offset is applied on top of `rect`/`icon_rect`
/// only at the moment they're pushed to DWM, painted, or hit-tested
/// (see `displayed_rect`).
pub(crate) struct ThumbAnim {
    pub(crate) hwnd: HWND,
    /// Used only for placeholder rendering.
    pub(crate) title: String,
    /// The window/app icon drawn at `icon_rect`, if one could be
    /// queried (`window_icon`) — owned by the window/class, never
    /// destroyed by this shell.
    pub(crate) icon: Option<HICON>,
    /// Which carousel page (workspace index, fixed for the lifetime of
    /// one overview session) this belongs to.
    pub(crate) page: usize,
    /// This window's grid-slot rect, aspect-fit within its cell.
    pub(crate) rect: RECT,
    /// Icon badge rect, directly below `rect`.
    pub(crate) icon_rect: RECT,
}

/// One workspace's card background rect — fixed once built (`page`
/// only ever moves via the carousel shift, never animated in place;
/// see the module docs on why the old "zoom from real position"
/// animation no longer applies once windows are grid-arranged instead
/// of position-accurate).
pub(crate) struct CardAnim {
    pub(crate) page: usize,
    pub(crate) rect: RECT,
}

/// An in-progress pointer drag through the carousel, started on
/// `WM_LBUTTONDOWN` while the overview is `Open`.
pub(crate) struct CarouselDrag {
    pub(crate) start_x: i32,
    pub(crate) start_offset: f64,
    /// Furthest horizontal distance from `start_x` seen so far, used at
    /// release time to distinguish a drag from a plain click.
    pub(crate) max_delta: i32,
}

/// A smooth, non-interactive slide of `carousel_offset` toward a target
/// page — used both for the drag-release "snap to nearest page" finish
/// and for keyboard/hotkey-triggered switches.
pub(crate) struct CarouselAnim {
    pub(crate) started: Instant,
    pub(crate) from: f64,
    pub(crate) to: f64,
}

pub(crate) enum OverviewMode {
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

/// One window preview being dragged between cards in the overview.
pub(crate) struct WindowDrag {
    pub(crate) hwnd: isize,
    pub(crate) from_page: usize,
    pub(crate) start_x: i32,
    pub(crate) start_y: i32,
    pub(crate) cur_x: i32,
    pub(crate) cur_y: i32,
    /// Peak pointer travel, for the click-vs-drag threshold (same idea
    /// as `CarouselDrag::max_delta`).
    pub(crate) max_delta: i32,
    /// Untransformed slot size — the key into the pre-scaled snapshot
    /// cache used to paint the drag ghost.
    pub(crate) base_w: i32,
    pub(crate) base_h: i32,
}

/// One row in the overview's search results.
enum SearchResult {
    /// An open window: activating switches to its workspace and
    /// focuses it.
    Window { hwnd: isize, title: String },
    /// An installed application (a Start Menu shortcut): activating
    /// launches it.
    App { name: String, path: std::path::PathBuf },
}

enum OverviewHit {
    Window { page: usize, hwnd: HWND },
    EmptyPage { page: usize },
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
pub(crate) fn card_layout() -> (RECT, i32) {
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

/// Re-syncs the workspace tracker (see `workspaces::sync_workspaces`)
/// and builds a fresh `(cards, thumbs)` pair covering *every* current
/// workspace's page — one wallpaper-filled card each (see
/// `card_layout`), each with the workspace's windows gridded inside it
/// (see `layout_grid`). Every preview is painted from a
/// `WINDOW_SNAPSHOTS` capture: parked windows already have their
/// park-time capture, and anything without one (the current
/// workspace's on-screen windows, typically) is captured here, while
/// its pixels are still available. Used both to open the overview and
/// to refresh it in place after a workspace switch
/// (`workspaces::commit_workspace_switch` calls this rather than
/// patching the previous session's pages piecemeal).
pub(crate) fn build_carousel_pages() -> (Vec<CardAnim>, Vec<ThumbAnim>, usize) {
    let live = super::workspaces::sync_workspaces();

    let (workspace_ids, current_pos): (Vec<groveshell_window_model::workspace::WorkspaceId>, usize) =
        STATE.with(|s| {
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
pub(crate) fn open_overview() {
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

/// Refreshes every page's cards/thumbnails in place (see
/// `build_carousel_pages`) without touching `carousel_offset` or
/// replaying the open animation — used after `commit_workspace_switch`
/// while the overview is already `Open`, since that can change which
/// workspace is current, how many workspaces exist (the dynamic-
/// workspace policy grows/shrinks the trailing empty one), and which
/// windows live where, all at once. Re-registers live thumbnails from
/// scratch rather than patching three kinds of drift piecemeal.
/// No-op if the overview isn't `Open`.
pub(crate) fn rebuild_open_overview_pages() {
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
pub(crate) fn close_overview(focus_after: Option<HWND>) {
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
                // Any in-progress drag/slide/search is moot once we're
                // fading back out.
                state.carousel_drag = None;
                state.carousel_anim = None;
                state.carousel_close_after = None;
                state.window_drag = None;
                state.search_query.clear();
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

/// Hit-tests a click against the overview's current thumbnail/card
/// rects across *every* carousel page (only meaningful while `Open`),
/// applying each page's current carousel shift first. Clicking a
/// window focuses it, switching to its workspace first if it isn't
/// already current. Clicking empty space on a *different* page just
/// re-centers the carousel there (like clicking a workspace thumbnail
/// in GNOME) without closing the overview. Clicking empty space on the
/// current page, or missing everything, cancels — the pre-existing
/// behavior.
pub(crate) fn on_overview_click(x: i32, y: i32) {
    // An active search owns clicks: a result row activates it, anywhere
    // else dismisses the search (but not the overview).
    let query = STATE.with(|s| {
        s.borrow()
            .as_ref()
            .filter(|st| !st.search_query.is_empty())
            .map(|st| st.search_query.clone())
    });
    if let Some(query) = query {
        let results = search_results(&query);
        let (_, rows) = search_layout(reference_dpi(), results.len());
        let hit_index = rows
            .iter()
            .skip(1)
            .position(|r| x >= r.left && x < r.right && y >= r.top && y < r.bottom);
        match hit_index {
            Some(i) => {
                if let Some(result) = results.into_iter().nth(i) {
                    activate_search_result(result);
                }
            }
            None => {
                STATE.with(|s| {
                    if let Some(state) = s.borrow_mut().as_mut() {
                        state.search_query.clear();
                    }
                });
                let overview_hwnd =
                    STATE.with(|s| s.borrow().as_ref().map(|st| st.overview_hwnd));
                if let Some(overview_hwnd) = overview_hwnd {
                    repaint_overview(overview_hwnd);
                }
            }
        }
        return;
    }

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

/// Starts tracking a drag; only takes effect while the overview is
/// idle-`Open` (matching the existing hit-testing gate — no dragging
/// mid zoom-animation). A press that lands on a window preview begins
/// a *window* drag (move it to another workspace card); anywhere else
/// begins a carousel drag.
pub(crate) fn on_overview_drag_start(x: i32, y: i32) {
    // The click that starts this drag just re-activated (and re-raised)
    // the overview — put the bars back on top of it.
    raise_bars_topmost();
    STATE.with(|s| {
        if let Some(state) = s.borrow_mut().as_mut() {
            if !matches!(state.overview, OverviewMode::Open { .. }) {
                return;
            }
            // While searching, presses belong to the results panel
            // (handled as clicks on release), not to dragging.
            if !state.search_query.is_empty() {
                return;
            }
            let thumb = {
                let OverviewMode::Open { thumbs, .. } = &state.overview else {
                    unreachable!()
                };
                let (card_rect, pitch) = card_layout();
                thumbs.iter().find_map(|th| {
                    let r = displayed_rect(th.rect, th.page, state.carousel_offset, pitch, card_rect);
                    (x >= r.left && x < r.right && y >= r.top && y < r.bottom).then(|| {
                        (
                            th.hwnd.0 as isize,
                            th.page,
                            th.rect.right - th.rect.left,
                            th.rect.bottom - th.rect.top,
                        )
                    })
                })
            };
            match thumb {
                Some((hwnd, from_page, base_w, base_h)) => {
                    state.window_drag = Some(WindowDrag {
                        hwnd,
                        from_page,
                        start_x: x,
                        start_y: y,
                        cur_x: x,
                        cur_y: y,
                        max_delta: 0,
                        base_w,
                        base_h,
                    });
                }
                None => {
                    state.carousel_drag = Some(CarouselDrag {
                        start_x: x,
                        start_offset: state.carousel_offset,
                        max_delta: 0,
                    });
                }
            }
        }
    });
}

/// Follows the pointer while a drag is active. A window drag just
/// tracks the cursor (the ghost is painted at the current position); a
/// carousel drag scrolls the content with the cursor (dragging right
/// reveals the *previous*, lower-index workspace), at a fixed
/// `CAROUSEL_DRAG_PAGE_DISTANCE_PX`-per-page rate rather than the
/// overview's actual (screen-spanning) pixel width — see that
/// constant's docs.
pub(crate) fn on_overview_drag_move(x: i32, y: i32) {
    let overview_hwnd = STATE.with(|s| {
        let mut state_ref = s.borrow_mut();
        let state = state_ref.as_mut()?;

        if let Some(drag) = state.window_drag.as_mut() {
            drag.cur_x = x;
            drag.cur_y = y;
            let travel = (x - drag.start_x).abs().max((y - drag.start_y).abs());
            drag.max_delta = drag.max_delta.max(travel);
            return Some(state.overview_hwnd);
        }

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

/// Ends whichever drag is active (or, if none — the button went down
/// outside `Open`, or never moved past the click threshold — dispatches
/// a plain click instead). A window drag released over a *different*
/// card moves the window to that workspace; a carousel drag snaps to
/// whichever page ended up nearest the release point.
pub(crate) fn on_overview_drag_end(x: i32, y: i32) {
    let window_drag = STATE.with(|s| s.borrow_mut().as_mut().and_then(|st| st.window_drag.take()));
    if let Some(drag) = window_drag {
        if drag.max_delta <= CAROUSEL_DRAG_CLICK_THRESHOLD_PX {
            on_overview_click(x, y);
        } else {
            on_window_drop(drag, x, y);
        }
        return;
    }

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

/// A window preview was dragged and released at `(x, y)`: if that's
/// over a different workspace's card, reassign the window there and
/// park/unpark it to match (it physically belongs on screen only while
/// its workspace is current). Released anywhere else, the drag just
/// cancels — the ghost disappears and the preview stays put.
fn on_window_drop(drag: WindowDrag, x: i32, y: i32) {
    let moved = STATE.with(|s| {
        let mut state_ref = s.borrow_mut();
        let state = state_ref.as_mut()?;
        let OverviewMode::Open { cards, .. } = &state.overview else {
            return None;
        };
        let (card_rect, pitch) = card_layout();
        let target = cards.iter().find_map(|c| {
            let r = displayed_rect(c.rect, c.page, state.carousel_offset, pitch, card_rect);
            (x >= r.left && x < r.right && y >= r.top && y < r.bottom).then_some(c.page)
        })?;
        if target == drag.from_page {
            return None;
        }
        state.workspaces.move_window_to_index(drag.hwnd, target)?;
        Some((target, state.workspaces.current_index()))
    });

    let overview_hwnd = STATE.with(|s| s.borrow().as_ref().map(|st| st.overview_hwnd));
    let Some((target, current)) = moved else {
        // Cancelled: just erase the ghost.
        if let Some(overview_hwnd) = overview_hwnd {
            repaint_overview(overview_hwnd);
        }
        return;
    };

    let hwnd = HWND(drag.hwnd as *mut c_void);
    if target == current {
        super::workspaces::unpark_window(hwnd);
    } else {
        super::workspaces::park_window(hwnd);
    }
    rebuild_open_overview_pages();
    refresh_bar_indicator();
}

/// Left/Right arrow keys while the overview is idle-`Open`: slide the
/// carousel by one workspace, same as the global hotkeys but without
/// leaving the overview.
pub(crate) fn on_overview_arrow(delta: i32) {
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

/// Commits to `target_index` immediately and starts a smooth visual
/// slide of the carousel to center on it (only if the overview is
/// idle-`Open`; otherwise the switch still happens, just with nothing
/// to animate). If `close_after` is `Some`, the overview closes
/// (focusing that window) once the slide lands.
pub(crate) fn snap_carousel_to(target_index: usize, close_after: Option<HWND>) {
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
pub(crate) fn repaint_overview(overview_hwnd: HWND) {
    // SAFETY: `overview_hwnd` is a valid, process-lifetime window.
    unsafe {
        let _ = InvalidateRect(overview_hwnd, None, false);
    }
}

thread_local! {
    /// Installed-application index for overview search: display name +
    /// launchable path for every Start Menu `.lnk`, both per-user and
    /// all-users. Built lazily on first search and kept for the process
    /// lifetime — apps installed mid-session just don't show up until
    /// restart, an acceptable staleness for a launcher.
    static APP_INDEX: RefCell<Option<Vec<(String, std::path::PathBuf)>>> =
        const { RefCell::new(None) };
}

fn collect_shortcuts(dir: &std::path::Path, out: &mut Vec<(String, std::path::PathBuf)>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_shortcuts(&path, out);
        } else if path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("lnk")) {
            if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                out.push((stem.to_string(), path));
            }
        }
    }
}

fn app_index_matches(query_lower: &str, limit: usize) -> Vec<(String, std::path::PathBuf)> {
    APP_INDEX.with(|cache| {
        let mut cache = cache.borrow_mut();
        let index = cache.get_or_insert_with(|| {
            let mut apps = Vec::new();
            for base in [
                std::env::var_os("APPDATA").map(std::path::PathBuf::from),
                std::env::var_os("ProgramData").map(std::path::PathBuf::from),
            ]
            .into_iter()
            .flatten()
            {
                collect_shortcuts(&base.join(r"Microsoft\Windows\Start Menu\Programs"), &mut apps);
            }
            apps.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
            apps.dedup_by(|a, b| a.0.eq_ignore_ascii_case(&b.0));
            apps
        });
        index
            .iter()
            .filter(|(name, _)| name.to_lowercase().contains(query_lower))
            .take(limit)
            .cloned()
            .collect()
    })
}

/// Open windows whose title or exe matches, then installed apps whose
/// name matches — capped at `SEARCH_MAX_RESULTS`, windows first since
/// switching beats launching a duplicate.
fn search_results(query: &str) -> Vec<SearchResult> {
    let q = query.to_lowercase();
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

    let mut results = Vec::new();
    for hwnd in tracked {
        let Some(record) = groveshell_window_model::describe(hwnd) else {
            continue;
        };
        let title_match = record.title.to_lowercase().contains(&q);
        let exe_match = record
            .exe_name
            .as_deref()
            .is_some_and(|e| e.to_lowercase().contains(&q));
        if title_match || exe_match {
            results.push(SearchResult::Window { hwnd, title: record.title });
        }
        if results.len() >= SEARCH_MAX_RESULTS {
            return results;
        }
    }
    for (name, path) in app_index_matches(&q, SEARCH_MAX_RESULTS - results.len()) {
        results.push(SearchResult::App { name, path });
    }
    results
}

/// The search panel's rects in overview-client coordinates: the panel
/// itself, then one rect per result row. Row 0 of `rows` is the header
/// line showing the query. Pure function of dpi + row count so painting
/// and click hit-testing can't disagree.
fn search_layout(dpi: u32, result_count: usize) -> (RECT, Vec<RECT>) {
    let (card, _) = card_layout();
    let width = scaled(SEARCH_PANEL_WIDTH, dpi);
    let row_h = scaled(SEARCH_ROW_HEIGHT, dpi);
    let left = (card.left + card.right) / 2 - width / 2;
    let top = scaled(BAR_HEIGHT + SEARCH_PANEL_GAP, dpi);
    let rows: Vec<RECT> = (0..result_count + 1)
        .map(|i| RECT {
            left,
            top: top + row_h * i as i32,
            right: left + width,
            bottom: top + row_h * (i as i32 + 1),
        })
        .collect();
    let panel = RECT {
        left,
        top,
        right: left + width,
        bottom: top + row_h * (result_count as i32 + 1),
    };
    (panel, rows)
}

fn activate_search_result(result: SearchResult) {
    STATE.with(|s| {
        if let Some(state) = s.borrow_mut().as_mut() {
            state.search_query.clear();
        }
    });
    match result {
        SearchResult::Window { hwnd, .. } => {
            let target = STATE.with(|s| {
                let state_ref = s.borrow();
                let st = state_ref.as_ref()?;
                let id = st.workspaces.workspace_of(hwnd)?;
                let page = st.workspaces.index_of(id)?;
                Some((page, st.workspaces.current_index()))
            });
            let handle = HWND(hwnd as *mut c_void);
            match target {
                Some((page, current)) if page != current => snap_carousel_to(page, Some(handle)),
                _ => close_overview(Some(handle)),
            }
        }
        SearchResult::App { path, .. } => {
            let wide: Vec<u16> = path
                .as_os_str()
                .encode_wide()
                .chain(std::iter::once(0))
                .collect();
            // SAFETY: `wide` is nul-terminated and outlives the call;
            // ShellExecuteW is fire-and-forget here.
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
            close_overview(None);
        }
    }
}

/// A printable keystroke (or backspace/Enter) while the overview is
/// open — GNOME-style type-to-search, no click into a box needed.
pub(crate) fn on_overview_char(ch: u32) {
    enum Action {
        Repaint,
        ActivateFirst(String),
        Nothing,
    }
    let (action, overview_hwnd) = STATE.with(|s| {
        let mut state_ref = s.borrow_mut();
        let Some(state) = state_ref.as_mut() else {
            return (Action::Nothing, None);
        };
        if !matches!(state.overview, OverviewMode::Open { .. }) {
            return (Action::Nothing, None);
        }
        let hwnd = state.overview_hwnd;
        match ch {
            0x08 => {
                state.search_query.pop();
                (Action::Repaint, Some(hwnd))
            }
            0x0D if !state.search_query.is_empty() => {
                (Action::ActivateFirst(state.search_query.clone()), Some(hwnd))
            }
            c => match char::from_u32(c).filter(|c| !c.is_control()) {
                Some(c) if state.search_query.chars().count() < 64 => {
                    state.search_query.push(c);
                    (Action::Repaint, Some(hwnd))
                }
                _ => (Action::Nothing, None),
            },
        }
    });
    match action {
        Action::ActivateFirst(query) => {
            if let Some(first) = search_results(&query).into_iter().next() {
                activate_search_result(first);
            }
            if let Some(overview_hwnd) = overview_hwnd {
                repaint_overview(overview_hwnd);
            }
        }
        Action::Repaint => {
            if let Some(overview_hwnd) = overview_hwnd {
                repaint_overview(overview_hwnd);
            }
        }
        Action::Nothing => {}
    }
}

pub(crate) fn paint_overview(hwnd: HWND) {
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
        let ghost = st
            .window_drag
            .as_ref()
            .filter(|d| d.max_delta > CAROUSEL_DRAG_CLICK_THRESHOLD_PX)
            .map(|d| (d.cur_x, d.cur_y, d.base_w, d.base_h, d.hwnd));
        let search = (!st.search_query.is_empty()).then(|| st.search_query.clone());
        Some((cards, snapshots, placeholders, icons, ghost, search))
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

        if let Some((cards, snapshots, placeholders, icons, ghost, search)) = content {
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

            // Type-to-search results panel, over everything else.
            if let Some(query) = &search {
                let results = search_results(query);
                let (panel, rows) = search_layout(dpi, results.len());
                draw_shadow(mem, panel, thumb_radius, 4);
                let panel_brush = CreateSolidBrush(COLORREF(0x002A2A2A));
                let null_pen = GetStockObject(NULL_PEN);
                let previous_brush = SelectObject(mem, panel_brush);
                let previous_pen = SelectObject(mem, null_pen);
                let _ = RoundRect(
                    mem,
                    panel.left,
                    panel.top,
                    panel.right,
                    panel.bottom,
                    thumb_radius * 2,
                    thumb_radius * 2,
                );
                SelectObject(mem, previous_pen);
                SelectObject(mem, previous_brush);
                let _ = DeleteObject(panel_brush);

                // First result gets a highlight — it's what Enter runs.
                if !results.is_empty() {
                    let hl_brush = CreateSolidBrush(COLORREF(0x003C3C3C));
                    FillRect(mem, &rows[1], hl_brush);
                    let _ = DeleteObject(hl_brush);
                }

                let font = bar_font(dpi);
                let previous_font = SelectObject(mem, font);
                SetBkMode(mem, TRANSPARENT);
                let pad = scaled(12, dpi);
                let inset = |r: &RECT| RECT {
                    left: r.left + pad,
                    top: r.top,
                    right: r.right - pad,
                    bottom: r.bottom,
                };
                SetTextColor(mem, COLORREF(0x00A0A0A0));
                draw_text_in(
                    mem,
                    inset(&rows[0]),
                    &format!("Search: {query}"),
                    DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS,
                );
                SetTextColor(mem, COLORREF(0x00E0E0E0));
                for (i, result) in results.iter().enumerate() {
                    let label = match result {
                        SearchResult::Window { title, .. } => title.clone(),
                        SearchResult::App { name, .. } => format!("{name}  (launch)"),
                    };
                    draw_text_in(
                        mem,
                        inset(&rows[i + 1]),
                        &label,
                        DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS,
                    );
                }
                SelectObject(mem, previous_font);
                let _ = DeleteObject(font);
            }

            // The dragged window's ghost, last so it rides above all.
            if let Some((cx, cy, base_w, base_h, drag_hwnd)) = ghost {
                if let Some(bitmap) = slot_scaled_snapshot(drag_hwnd, base_w, base_h) {
                    let gw = base_w * 3 / 5;
                    let gh = base_h * 3 / 5;
                    let rect = RECT {
                        left: cx - gw / 2,
                        top: cy - gh / 2,
                        right: cx + gw / 2,
                        bottom: cy + gh / 2,
                    };
                    draw_shadow(mem, rect, thumb_radius, 4);
                    let src = CreateCompatibleDC(hdc);
                    let previous_src = SelectObject(src, HBITMAP(bitmap as *mut c_void));
                    let _ = StretchBlt(
                        mem,
                        rect.left,
                        rect.top,
                        gw,
                        gh,
                        src,
                        0,
                        0,
                        base_w,
                        base_h,
                        SRCCOPY,
                    );
                    SelectObject(src, previous_src);
                    let _ = DeleteDC(src);
                }
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
            let _ = DeleteObject(HBITMAP(handle as *mut c_void));
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
            SelectObject(mem, HBITMAP(handle as *mut c_void));
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

pub(crate) fn window_snapshot(hwnd: isize) -> Option<(i32, i32, isize)> {
    WINDOW_SNAPSHOTS.with(|s| s.borrow().get(&hwnd).copied())
}

pub(crate) fn drop_window_snapshot(hwnd: isize) {
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
pub(crate) fn retain_window_snapshots(tracked: &[isize]) {
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
pub(crate) fn capture_window_snapshot(hwnd: HWND) {
    // SAFETY: create-select-print-restore on locally created GDI
    // handles; `bitmap`'s ownership moves into the store on success.
    unsafe {
        let mut rect = RECT::default();
        if windows::Win32::UI::WindowsAndMessaging::GetWindowRect(hwnd, &mut rect).is_err() {
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

/// Advances whichever animation is in flight by one tick: the
/// `Opening`/`Closing` whole-window fade, and/or an independent
/// in-flight carousel slide (these can run at the same time, or a
/// carousel slide can run on its own while the overview just sits
/// `Open`) — or finalizes either once it reaches the end.
pub(crate) fn on_animation_tick() {
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
