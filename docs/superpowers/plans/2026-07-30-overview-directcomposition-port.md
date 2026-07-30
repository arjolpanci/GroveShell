# Activities Overview DirectComposition Port Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Port the Activities overview's carousel motion and open/close fade from full-scene GDI redraws to a per-card DirectComposition/Direct2D visual tree, so both are GPU-composited and near-zero-CPU when idle, matching what Phase 1 already proved for the calendar flyout.

**Architecture:** Each monitor's `OverviewInstance` gains an optional `OverviewGpuState`: one root `IDCompositionVisual2` (dock + search chrome + drag ghost, one shared surface) plus one child `IDCompositionVisual2` per `CardAnim` (frame/wallpaper/shadow/thumbnails/icons/chips, its own surface at natural unscrolled size). Carousel slide/zoom becomes a `SetTransform2` call per card visual every tick; open/close fade becomes `SetOpacity2` on the root visual. Card/root *content* is repainted only when it actually changes (page rebuild, hover state change, dock/search state change) — never on every tick. If GPU setup fails for a specific overview window, that monitor's overview keeps calling today's GDI `paint_overview`/`repaint_overview` entirely unchanged.

**Tech Stack:** `windows` crate 0.58 (Direct2D, DirectComposition, DirectWrite, WIC, Direct3D11), Rust, existing GDI painting code as the untouched fallback path.

## Global Constraints

- Never add `Co-Authored-By` trailers to any commit (standing project rule).
- The GPU-available decision is made once per process at startup (`gpu::init`, already implemented) and never revisited; this plan only adds *new* per-overview-window setup that itself can independently fail and fall back, exactly like the calendar's `create_surface`.
- `unsafe` blocks around Win32/COM calls must carry a `// SAFETY:` comment, per the existing convention in `gpu.rs`, `calendar.rs`, and `overview.rs`.
- No new dependency on real `IDCompositionAnimation` compositor timelines — the app's existing 16ms `ANIM_TIMER` and `CarouselAnim`/`OverviewMode`/`WindowPopAnim` state machines stay structurally unchanged (see the design doc's "Option A").
- Drag-pop ghost, hover glow, search box, and dock rendering are explicitly in scope only as "port their existing drawing to Direct2D, targeting the new root/card surfaces" — not to be made compositor-animated or otherwise optimized further in this plan.
- Every task must leave `cargo build --workspace` and `cargo clippy -p groveshell-ui --no-deps` clean before committing.

---

### Task 1: Upgrade `gpu.rs` to `IDCompositionDesktopDevice`/`IDCompositionVisual2`, keep the D2D factory

The calendar's existing `IDCompositionDevice`/`IDCompositionVisual` can create surfaces and target a `HWND`, but can't create visuals with opacity control (`SetOpacity2`, needed for the overview's root visual). `IDCompositionDesktopDevice` derefs to `IDCompositionDevice2` (whose `CreateVisual` returns `IDCompositionVisual2`) while still owning its own `CreateTargetForHwnd` — confirmed against the vendored `windows-0.58.0` DirectComposition bindings. This task swaps the type with no behavior change for the calendar's existing calls (`CreateVisual`/`CreateSurface`/`Commit`/`CreateTargetForHwnd` all still resolve).

**Files:**
- Modify: `apps/ui/src/imp/gpu.rs:19,23-26,45-71,88-126`

**Interfaces:**
- Produces: `GpuContext.dcomp_device: IDCompositionDesktopDevice`, `GpuContext.d2d_factory: ID2D1Factory1` (new field), `GpuSurface.visual: IDCompositionVisual2` (was `IDCompositionVisual`), `pub(crate) fn set_opacity(surface: &GpuSurface, opacity: f32)`.

- [ ] **Step 1: Change the import and `GpuContext` struct**

```rust
use windows::Win32::Graphics::DirectComposition::{DCompositionCreateDevice2, IDCompositionDesktopDevice};
```//... replaces the old `IDCompositionDevice` import at gpu.rs:19

```rust
pub(crate) struct GpuContext {
    pub(crate) dcomp_device: IDCompositionDesktopDevice,
    pub(crate) d2d_factory: ID2D1Factory1,
    pub(crate) dwrite_factory: IDWriteFactory,
}
```

- [ ] **Step 2: Update `try_init` to keep the factory and request the desktop device**

```rust
fn try_init() -> windows::core::Result<GpuContext> {
    // SAFETY: plain device/factory creation; every output parameter is a
    // local on the stack, valid to write to for the duration of the call.
    unsafe {
        let mut d3d_device = None;
        D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            None,
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            None,
            D3D11_SDK_VERSION,
            Some(&mut d3d_device),
            None,
            None,
        )?;
        let d3d_device = d3d_device.expect("D3D11CreateDevice succeeded but returned no device");
        let dxgi_device: IDXGIDevice = d3d_device.cast()?;

        let d2d_factory: ID2D1Factory1 = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
        let d2d_device: ID2D1Device = d2d_factory.CreateDevice(&dxgi_device)?;

        let dcomp_device: IDCompositionDesktopDevice = DCompositionCreateDevice2(&d2d_device)?;
        let dwrite_factory: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;

        Ok(GpuContext { dcomp_device, d2d_factory, dwrite_factory })
    }
}
```

- [ ] **Step 3: Update `GpuSurface`/`try_create_surface`'s visual type**

Change the `visual: IDCompositionVisual` field (gpu.rs:92) to `visual: IDCompositionVisual2`, and the local `let visual = ctx.dcomp_device.CreateVisual()?;` in `try_create_surface` now infers `IDCompositionVisual2` automatically (no call-site change needed — `CreateVisual` on `IDCompositionDesktopDevice` resolves via `Deref` to `IDCompositionDevice2::CreateVisual`, which returns `IDCompositionVisual2`). `target.SetRoot(&visual)?` still compiles unchanged: `SetRoot` takes anything satisfying `Param<IDCompositionVisual>`, and `IDCompositionVisual2` derefs to `IDCompositionVisual`.

- [ ] **Step 4: Add an opacity-setting wrapper**

```rust
/// Sets `surface`'s root visual opacity (0.0–1.0) and commits. No-op if
/// the process-wide GPU setup isn't available.
pub(crate) fn set_opacity(surface: &GpuSurface, opacity: f32) {
    GPU.with(|g| {
        let g = g.borrow();
        let Some(ctx) = g.as_ref() else { return };
        // SAFETY: `surface.visual` was created by this module's own
        // `try_create_surface` and is alive for as long as the caller's
        // `GpuSurface` is.
        unsafe {
            let _ = surface.visual.SetOpacity2(opacity);
            let _ = ctx.dcomp_device.Commit();
        }
    });
}
```

- [ ] **Step 5: Build and lint**

Run: `cargo build --workspace` — expect clean (the calendar's existing calls all still typecheck against the new interfaces per Step 3's reasoning).
Run: `cargo clippy -p groveshell-ui --no-deps` — expect clean.

- [ ] **Step 6: Commit**

```bash
git add apps/ui/src/imp/gpu.rs
git commit -m "feat: upgrade the GPU device to IDCompositionDesktopDevice for visual opacity"
```

---

### Task 2: Add Direct2D primitives needed for card/root content (bitmaps, rounded clip, stroke)

The calendar only ever needed flat fills and text. Overview cards need: a WIC-bridged bitmap draw (wallpaper, window thumbnails, icons — all start life as a GDI `HBITMAP`/`HICON`), a rounded-rect clip (cards and thumbnails have rounded corners), and a stroked rounded rect (shadow/hover-glow, simplified from the GDI code's multi-ring approximation to one solid semi-transparent stroke — visually similar, much less per-redraw cost, and not the animation hot path per the design's scope).

**Files:**
- Modify: `apps/ui/Cargo.toml:18-50` (add `"Win32_Graphics_Imaging"`)
- Modify: `apps/ui/src/imp/gpu.rs` (append new functions)

**Interfaces:**
- Consumes: `GpuContext.d2d_factory` (Task 1).
- Produces: `pub(crate) fn bitmap_from_hbitmap(ctx: &ID2D1DeviceContext, hbitmap: windows::Win32::Graphics::Gdi::HBITMAP) -> Option<ID2D1Bitmap>`, `pub(crate) fn draw_rounded_bitmap(ctx: &ID2D1DeviceContext, rect: D2D_RECT_F, radius: f32, bitmap: &ID2D1Bitmap)`, `pub(crate) fn fill_rounded_rect(ctx: &ID2D1DeviceContext, rect: D2D_RECT_F, radius: f32, colorref: u32)`, `pub(crate) fn stroke_rounded_rect(ctx: &ID2D1DeviceContext, rect: D2D_RECT_F, radius: f32, colorref: u32, alpha: f32, stroke_width: f32)`.

- [ ] **Step 1: Add the `Win32_Graphics_Imaging` feature**

```toml
  "Win32_Graphics_Direct2D",
  "Win32_Graphics_Direct2D_Common",
  "Win32_Graphics_Direct3D",
  "Win32_Graphics_Direct3D11",
  "Win32_Graphics_DirectComposition",
  "Win32_Graphics_DirectWrite",
  "Win32_Graphics_Dwm",
  "Win32_Graphics_Dxgi",
  "Win32_Graphics_Dxgi_Common",
  "Win32_Graphics_Gdi",
  "Win32_Graphics_GdiPlus",
  "Win32_Graphics_Imaging",
```
(inserted alphabetically after `Win32_Graphics_GdiPlus`)

- [ ] **Step 2: WIC bridge — HBITMAP to ID2D1Bitmap**

Append to `gpu.rs`:

```rust
use windows::Win32::Graphics::Gdi::HBITMAP;
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, IWICImagingFactory, WICBitmapUsePremultipliedAlpha,
};
use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};

/// Converts a GDI `HBITMAP` (a `PrintWindow` capture, a wallpaper tile,
/// or an icon rendered to a temp bitmap) into a Direct2D-drawable
/// bitmap via the WIC bridge. `None` on any failure — callers already
/// treat a missing bitmap as "draw the placeholder chip instead",
/// matching today's GDI behavior.
pub(crate) fn bitmap_from_hbitmap(ctx: &ID2D1DeviceContext, hbitmap: HBITMAP) -> Option<ID2D1Bitmap> {
    // SAFETY: `hbitmap` is a valid, caller-owned GDI bitmap for the
    // duration of this call; every COM object created here is released
    // when it goes out of scope at the end of the function.
    unsafe {
        let wic: IWICImagingFactory =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok()?;
        let wic_bitmap = wic
            .CreateBitmapFromHBITMAP(hbitmap, None, WICBitmapUsePremultipliedAlpha)
            .ok()?;
        ctx.CreateBitmapFromWicBitmap(&wic_bitmap, None).ok()
    }
}

/// Draws `bitmap` stretched to fill `rect`, clipped to a rounded rect
/// of `radius`. Mirrors `overview.rs`'s GDI `StretchBlt`-into-a-
/// `CreateRoundRectRgn`-clip pattern.
pub(crate) fn draw_rounded_bitmap(ctx: &ID2D1DeviceContext, rect: D2D_RECT_F, radius: f32, bitmap: &ID2D1Bitmap) {
    // SAFETY: `ctx` is a live device context between `BeginDraw`/`EndDraw`.
    unsafe {
        let geometry = GPU.with(|g| {
            let g = g.borrow();
            let ctx = g.as_ref()?;
            ctx.d2d_factory
                .CreateRoundedRectangleGeometry(&D2D1_ROUNDED_RECT {
                    rect,
                    radiusX: radius,
                    radiusY: radius,
                })
                .ok()
        });
        let Some(geometry) = geometry else { return };
        let Ok(layer) = ctx.CreateLayer(None) else { return };
        let params = D2D1_LAYER_PARAMETERS1 {
            contentBounds: rect,
            geometricMask: core::mem::ManuallyDrop::new(Some(geometry)),
            maskAntialiasMode: D2D1_ANTIALIAS_MODE_PER_PRIMITIVE,
            maskTransform: Matrix3x2 { M11: 1.0, M12: 0.0, M21: 0.0, M22: 1.0, M31: 0.0, M32: 0.0 },
            opacity: 1.0,
            opacityBrush: core::mem::ManuallyDrop::new(None),
            layerOptions: D2D1_LAYER_OPTIONS1_NONE,
        };
        if ctx.PushLayer(&params, &layer).is_ok() {
            let _ = ctx.DrawBitmap(bitmap, Some(&rect), 1.0, D2D1_INTERPOLATION_MODE_LINEAR, None, None);
            let _ = ctx.PopLayer();
        }
        // `D2D1_LAYER_PARAMETERS1.geometricMask` is a `ManuallyDrop` —
        // its COM reference must be released explicitly or every call
        // leaks one `ID2D1Geometry` (this runs on every card
        // redraw/thumbnail, not once at startup, so the leak would be
        // real).
        let mut params = params;
        core::mem::ManuallyDrop::drop(&mut params.geometricMask);
    }
}

/// Fills a rounded rect — the flat-color fallback drawn under the
/// wallpaper (mirrors the GDI fallback-brush fill in `paint_overview`).
pub(crate) fn fill_rounded_rect(ctx: &ID2D1DeviceContext, rect: D2D_RECT_F, radius: f32, colorref: u32) {
    // SAFETY: `ctx` is a live device context between `BeginDraw`/`EndDraw`.
    unsafe {
        let geometry = GPU.with(|g| {
            let g = g.borrow();
            let ctx = g.as_ref()?;
            ctx.d2d_factory
                .CreateRoundedRectangleGeometry(&D2D1_ROUNDED_RECT { rect, radiusX: radius, radiusY: radius })
                .ok()
        });
        let Some(geometry) = geometry else { return };
        if let Ok(brush) = ctx.CreateSolidColorBrush(&colorref_to_d2d(colorref), None) {
            ctx.FillGeometry(&geometry, &brush, None);
        }
    }
}

/// A single semi-transparent rounded stroke, just inside `rect`'s
/// edge — the Direct2D replacement for the GDI multi-ring shadow/glow
/// approximation. `alpha` (0..1) drives fade-in for the hover glow;
/// shadow callers always pass a fixed alpha.
pub(crate) fn stroke_rounded_rect(
    ctx: &ID2D1DeviceContext,
    rect: D2D_RECT_F,
    radius: f32,
    colorref: u32,
    alpha: f32,
    stroke_width: f32,
) {
    // SAFETY: `ctx` is a live device context between `BeginDraw`/`EndDraw`.
    unsafe {
        let inset = stroke_width / 2.0;
        let inset_rect = D2D_RECT_F {
            left: rect.left + inset,
            top: rect.top + inset,
            right: rect.right - inset,
            bottom: rect.bottom - inset,
        };
        let Ok(geometry) = GPU.with(|g| {
            let g = g.borrow();
            let ctx = g.as_ref().expect("stroke_rounded_rect called with no GPU context");
            ctx.d2d_factory.CreateRoundedRectangleGeometry(&D2D1_ROUNDED_RECT {
                rect: inset_rect,
                radiusX: radius,
                radiusY: radius,
            })
        }) else {
            return;
        };
        let mut color = colorref_to_d2d(colorref);
        color.a = alpha.clamp(0.0, 1.0);
        if let Ok(brush) = ctx.CreateSolidColorBrush(&color, None) {
            let _ = ctx.DrawGeometry(&geometry, &brush, stroke_width, None);
        }
    }
}
```

- [ ] **Step 3: Add the new imports these functions need**

```rust
use windows::Win32::Graphics::Direct2D::Common::D2D1_ROUNDED_RECT;
use windows::Win32::Graphics::Direct2D::{
    D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_INTERPOLATION_MODE_LINEAR, D2D1_LAYER_OPTIONS1_NONE,
    D2D1_LAYER_PARAMETERS1, ID2D1Bitmap,
};
```

- [ ] **Step 4: Build and lint**

Run: `cargo build --workspace` — fix any signature mismatches against the exact windows-0.58.0 vtable signatures (already verified against the vendored source for this plan).
Run: `cargo clippy -p groveshell-ui --no-deps` — expect clean.

- [ ] **Step 5: Commit**

```bash
git add apps/ui/Cargo.toml apps/ui/src/imp/gpu.rs
git commit -m "feat: add Direct2D bitmap/rounded-rect primitives for the overview port"
```

---

### Task 3: `overview_gpu.rs` — visual tree state and lifecycle

New module holding the overview's GPU state shape and the functions that build/rebuild the per-card visual list to match the current `CardAnim` set. No painting yet — that's Task 5.

**Files:**
- Create: `apps/ui/src/imp/overview_gpu.rs`
- Modify: `apps/ui/src/imp/mod.rs:1-30` (add `mod overview_gpu;`)

**Interfaces:**
- Consumes: `super::gpu::{GpuSurface, create_surface}` (existing), `overview::CardAnim` (existing).
- Produces: `pub(crate) struct OverviewGpuState { root: GpuSurface, cards: Vec<CardVisual> }`, `pub(crate) struct CardVisual { pub(crate) page: usize, pub(crate) surface: GpuSurface }`, `pub(crate) fn create(hwnd: HWND, width: i32, height: i32) -> Option<OverviewGpuState>`, `pub(crate) fn rebuild_cards(state: &mut OverviewGpuState, cards: &[CardAnim])`.

- [ ] **Step 1: Write the module**

```rust
//! GPU-composited visual tree for one monitor's Activities overview —
//! Phase 2 of the animation-fluidity work (see
//! `docs/superpowers/specs/2026-07-29-overview-directcomposition-port-design.md`).
//! One root visual (dock/search/ghost chrome) plus one child visual per
//! current `CardAnim` (frame/wallpaper/shadow/thumbnails/icons/chips).
//! `None`/absent anywhere in this module means "fall back to this
//! overview window's existing GDI painting, unchanged" — never a panic.

use windows::Win32::Foundation::HWND;

use super::gpu::{self, GpuSurface};
use super::overview::CardAnim;

/// One workspace card's own composited surface, at the card's natural
/// (unscrolled, unzoomed) size — carousel position/zoom is applied on
/// top via `SetTransform2`, never baked into what's drawn here.
pub(crate) struct CardVisual {
    pub(crate) page: usize,
    pub(crate) surface: GpuSurface,
}

/// One monitor overview's GPU state. `root`'s surface holds the dock
/// bar, search panel, and drag ghost; `cards` holds one entry per
/// current `CardAnim`, kept in sync by `rebuild_cards`.
pub(crate) struct OverviewGpuState {
    pub(crate) root: GpuSurface,
    pub(crate) cards: Vec<CardVisual>,
}

/// Creates the root surface for `hwnd` (sized to the overview window's
/// full client area). Returns `None` if the process-wide GPU setup
/// isn't available, or if this specific window's target/surface setup
/// fails — either way the caller keeps using GDI for this monitor's
/// overview, unchanged.
pub(crate) fn create(hwnd: HWND, width: i32, height: i32) -> Option<OverviewGpuState> {
    let root = gpu::create_surface(hwnd, width, height)?;
    Some(OverviewGpuState { root, cards: Vec::new() })
}

/// Rebuilds `state.cards` to have exactly one entry per `cards`,
/// reusing each existing card visual's surface when a page's rect size
/// hasn't changed (avoids a needless surface recreation on every
/// `build_carousel_pages` call when only window contents changed, not
/// layout). Cards are recreated in `cards`' order, matching how
/// `on_animation_tick`'s per-tick transform pass below will iterate
/// them.
pub(crate) fn rebuild_cards(state: &mut OverviewGpuState, hwnd: HWND, cards: &[CardAnim]) {
    let mut rebuilt = Vec::with_capacity(cards.len());
    for card in cards {
        let (w, h) = (card.rect.right - card.rect.left, card.rect.bottom - card.rect.top);
        let reused = state
            .cards
            .iter()
            .position(|cv| cv.page == card.page)
            .map(|i| state.cards.remove(i))
            .filter(|cv| cv.surface.width() == w && cv.surface.height() == h);
        let card_visual = match reused {
            Some(cv) => cv,
            None => match gpu::create_surface(hwnd, w.max(1), h.max(1)) {
                Some(surface) => CardVisual { page: card.page, surface },
                None => continue,
            },
        };
        rebuilt.push(card_visual);
    }
    state.cards = rebuilt;
}
```

- [ ] **Step 2: Register the module**

In `apps/ui/src/imp/mod.rs`, add `mod overview_gpu;` alongside the existing `mod overview;` declaration (alphabetically adjacent).

- [ ] **Step 3: Add `GpuSurface::width`/`height`**

`rebuild_cards` needs to know a reused surface's size. Add to `apps/ui/src/imp/gpu.rs`'s `GpuSurface` impl (create one if it doesn't exist yet):

```rust
impl GpuSurface {
    pub(crate) fn width(&self) -> i32 {
        self.width
    }
    pub(crate) fn height(&self) -> i32 {
        self.height
    }
}
```

This requires `GpuSurface` to actually store its size — add `width: i32, height: i32` fields, set them in `try_create_surface` (`Ok(GpuSurface { target, visual, surface, width, height })`).

- [ ] **Step 4: Build and lint**

Run: `cargo build --workspace` — expect clean (new module, no call sites wired yet, so nothing else changes behavior).
Run: `cargo clippy -p groveshell-ui --no-deps` — expect clean; `OverviewGpuState`/`CardVisual`/`create`/`rebuild_cards` will show `dead_code` warnings until Task 4 wires them in — add `#[allow(dead_code)]` at the module level for now, to be removed implicitly once Task 4 lands (its diff will make the warning-suppression unnecessary, but leaving the attribute is harmless — remove it as part of Task 4's diff for cleanliness).

- [ ] **Step 5: Commit**

```bash
git add apps/ui/src/imp/overview_gpu.rs apps/ui/src/imp/mod.rs apps/ui/src/imp/gpu.rs
git commit -m "feat: add the overview's per-card DirectComposition visual state"
```

---

### Task 4: Wire GPU state into `OverviewInstance` and creation sites

Give every `OverviewInstance` a `gpu` field, and create it at both places an overview window is created: startup (`mod.rs`) and hotplug monitor-add (`hotplug.rs`). Teardown needs no explicit code — dropping `OverviewInstance` drops `OverviewGpuState`, which drops every `GpuSurface`, which releases its COM objects automatically (same reasoning already established for the calendar).

**Files:**
- Modify: `apps/ui/src/imp/overview.rs:168-203` (`OverviewInstance` struct + `new`)
- Modify: `apps/ui/src/imp/mod.rs:261-280` (startup overview creation loop)
- Modify: `apps/ui/src/imp/hotplug.rs:135-175` (hotplug monitor-add)

**Interfaces:**
- Consumes: `overview_gpu::{OverviewGpuState, create}` (Task 3).
- Produces: `OverviewInstance.gpu: Option<OverviewGpuState>`.

- [ ] **Step 1: Add the field**

In `apps/ui/src/imp/overview.rs`, add to the `OverviewInstance` struct (after `search_query`):

```rust
    pub(crate) search_query: String,
    /// `None` if GPU rendering isn't available for this window (see
    /// `gpu::is_enabled`) or this specific window's DirectComposition
    /// setup failed — `paint_overview`/`repaint_overview`/
    /// `on_animation_tick` all fall back to plain GDI in that case,
    /// unchanged from before this feature existed.
    pub(crate) gpu: Option<super::overview_gpu::OverviewGpuState>,
```

And to `OverviewInstance::new`, change the signature and body:

```rust
impl OverviewInstance {
    pub(crate) fn new(hwnd: HWND, width: i32, height: i32) -> Self {
        Self {
            hwnd,
            mode: OverviewMode::Closed,
            carousel_offset: 0.0,
            carousel_drag: None,
            carousel_anim: None,
            carousel_close_after: None,
            window_drag: None,
            window_pop_anim: None,
            hover_thumb: None,
            dock_apps: Vec::new(),
            dock_hover: None,
            search_query: String::new(),
            gpu: super::overview_gpu::create(hwnd, width, height),
        }
    }
}
```

- [ ] **Step 2: Update the startup call site**

In `apps/ui/src/imp/mod.rs`, the existing loop already computes `width`/`height` right before `CreateWindowExW` (mod.rs:262-263). Change:

```rust
overviews.insert(monitor.device_name.clone(), overview::OverviewInstance::new(overview_hwnd));
```
to
```rust
overviews.insert(
    monitor.device_name.clone(),
    overview::OverviewInstance::new(overview_hwnd, width, height),
);
```

- [ ] **Step 3: Update the hotplug call site**

In `apps/ui/src/imp/hotplug.rs`, `overview_width`/`overview_height` are already computed right before `CreateWindowExW` (hotplug.rs:135-136). Change:

```rust
state.overviews.insert(monitor.device_name.clone(), OverviewInstance::new(overview_hwnd));
```
to
```rust
state.overviews.insert(
    monitor.device_name.clone(),
    OverviewInstance::new(overview_hwnd, overview_width, overview_height),
);
```

- [ ] **Step 4: Build and lint**

Run: `cargo build --workspace` — expect clean.
Run: `cargo clippy -p groveshell-ui --no-deps` — expect clean; this is also the point to remove the `#[allow(dead_code)]` added in Task 3 Step 4, since `create`/`OverviewGpuState` are now genuinely used.

- [ ] **Step 5: Commit**

```bash
git add apps/ui/src/imp/overview.rs apps/ui/src/imp/mod.rs apps/ui/src/imp/hotplug.rs
git commit -m "feat: create the overview's GPU visual state alongside its window"
```

---

### Task 5: Card content painting (frame, wallpaper, shadow, thumbnails, icons, chips)

Port the per-card slice of `paint_overview`'s GDI drawing to Direct2D, targeting one card's own surface at its natural size. Wired in at the two places card content is (re)built today: `build_carousel_pages` (open) and `rebuild_open_overview_pages` (already-open refresh) — both already call `card_layout`/`layout_grid` per page, so this task adds a GPU-content-paint pass alongside the existing thumb/card list construction, gated on `gpu.is_some()`.

**Files:**
- Create: append to `apps/ui/src/imp/overview_gpu.rs`
- Modify: `apps/ui/src/imp/overview.rs:511-586` (`build_carousel_pages`), `:657-683` (`rebuild_open_overview_pages`)

**Interfaces:**
- Consumes: `gpu::{fill_rect, draw_text, bitmap_from_hbitmap, draw_rounded_bitmap, fill_rounded_rect, stroke_rounded_rect, redraw}` (Tasks 1–2), `overview::{ThumbAnim, CardAnim, window_snapshot, slot_scaled_snapshot, scaled, reference_dpi}` (existing), `overview_gpu::{OverviewGpuState, CardVisual}` (Task 3).
- Produces: `pub(crate) fn paint_card(card_visual: &CardVisual, card: &CardAnim, thumbs: &[&ThumbAnim], monitor: &str)`.

- [ ] **Step 1: Write `paint_card`**

Append to `overview_gpu.rs`:

```rust
use std::ffi::c_void;

use windows::Win32::Foundation::{COLORREF, HWND, RECT};
use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;
use windows::Win32::Graphics::Direct2D::ID2D1DeviceContext;
use windows::Win32::Graphics::Gdi::{
    CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject, DrawIconEx, GetDC, ReleaseDC,
    SelectObject, HBITMAP, DI_NORMAL,
};
use windows::Win32::UI::WindowsAndMessaging::HICON;

use super::gpu;
use super::overview::{scaled, ThumbAnim};
use super::state::{reference_dpi, BAR_HEIGHT};

fn rect_to_d2d(r: RECT, origin_x: i32, origin_y: i32) -> D2D_RECT_F {
    D2D_RECT_F {
        left: (r.left - origin_x) as f32,
        top: (r.top - origin_y) as f32,
        right: (r.right - origin_x) as f32,
        bottom: (r.bottom - origin_y) as f32,
    }
}

/// Renders an `HICON` to a small ARGB `HBITMAP` so it can go through
/// the same WIC bridge as window thumbnails and the wallpaper — icons
/// have no separate Direct2D-native path.
fn icon_to_hbitmap(icon: HICON, size: i32) -> Option<HBITMAP> {
    // SAFETY: standard create-select-draw-restore GDI sequence on
    // locally created handles; `bitmap`'s ownership moves to the
    // caller on success.
    unsafe {
        let screen = GetDC(None);
        let mem = CreateCompatibleDC(screen);
        let bitmap = CreateCompatibleBitmap(screen, size, size);
        let previous = SelectObject(mem, bitmap);
        let ok = DrawIconEx(mem, 0, 0, icon, size, size, 0, None, DI_NORMAL).is_ok();
        SelectObject(mem, previous);
        let _ = DeleteDC(mem);
        ReleaseDC(None, screen);
        if !ok {
            let _ = DeleteObject(bitmap);
            return None;
        }
        Some(bitmap)
    }
}

/// Paints one card's frame (wallpaper-filled rounded rect, drop-shadow
/// approximation), thumbnails (each clipped/rounded, WIC-bridged from
/// the existing `PrintWindow` capture cache), icon badges, and
/// placeholder chips — mirrors the corresponding subset of
/// `overview::paint_overview`'s GDI drawing, at the card's own natural
/// (unscrolled) size. `thumbs` must already be filtered to this card's
/// `page`.
pub(crate) fn paint_card(card_visual: &CardVisual, card_rect: RECT, thumbs: &[&ThumbAnim], monitor: &str) {
    let dpi = reference_dpi();
    let card_radius = scaled(20, dpi) as f32; // CARD_CORNER_RADIUS
    let thumb_radius = scaled(8, dpi) as f32; // THUMB_CORNER_RADIUS
    let (origin_x, origin_y) = (card_rect.left, card_rect.top);
    let full = rect_to_d2d(card_rect, origin_x, origin_y);

    gpu::redraw(&card_visual.surface, |ctx: &ID2D1DeviceContext| {
        // Flat fallback fill (shows through if the wallpaper bridge
        // below fails), then the wallpaper itself.
        gpu::fill_rounded_rect(ctx, full, card_radius, 0x00302010);
        if let Some(wallpaper) = super::overview::wallpaper_hbitmap_for(monitor, card_rect) {
            if let Some(bitmap) = gpu::bitmap_from_hbitmap(ctx, wallpaper) {
                gpu::draw_rounded_bitmap(ctx, full, card_radius, &bitmap);
            }
        }

        for th in thumbs {
            let rect = rect_to_d2d(th.rect, origin_x, origin_y);
            let (base_w, base_h) = (th.rect.right - th.rect.left, th.rect.bottom - th.rect.top);
            let hwnd = th.hwnd.0 as isize;
            if super::overview::window_snapshot(hwnd).is_some() {
                if let Some(scaled_handle) = super::overview::slot_scaled_snapshot(hwnd, base_w, base_h) {
                    if let Some(bitmap) =
                        gpu::bitmap_from_hbitmap(ctx, HBITMAP(scaled_handle as *mut c_void))
                    {
                        gpu::draw_rounded_bitmap(ctx, rect, thumb_radius, &bitmap);
                    }
                }
            } else {
                gpu::fill_rounded_rect(ctx, rect, thumb_radius, 0x00303030);
                gpu::draw_text(ctx, rect, &th.title, 0x00E0E0E0, 13.0, true);
            }
            if let Some(icon) = th.icon {
                let icon_rect = rect_to_d2d(th.icon_rect, origin_x, origin_y);
                let size = (th.icon_rect.right - th.icon_rect.left).max(1);
                if let Some(icon_bitmap) = icon_to_hbitmap(icon, size) {
                    if let Some(bitmap) = gpu::bitmap_from_hbitmap(ctx, icon_bitmap) {
                        gpu::draw_rounded_bitmap(ctx, icon_rect, 0.0, &bitmap);
                    }
                    // SAFETY: `icon_bitmap` was created locally above by
                    // `icon_to_hbitmap` and is owned exclusively here.
                    unsafe {
                        let _ = DeleteObject(icon_bitmap);
                    }
                }
            }
        }
    });
}
```

- [ ] **Step 2: Expose a wallpaper-as-HBITMAP accessor**

`overview.rs`'s wallpaper caching (`scaled_wallpaper`) already returns an `isize` HBITMAP handle keyed by size. Add a thin public wrapper next to `draw_wallpaper_into` in `overview.rs`:

```rust
/// The wallpaper pre-scaled to `rect`'s size (see `scaled_wallpaper`),
/// as a raw GDI handle for the GPU path's WIC bridge. `None` if the
/// wallpaper couldn't be loaded — callers already handle that as
/// "leave the flat fallback fill showing."
pub(crate) fn wallpaper_hbitmap_for(monitor: &str, rect: RECT) -> Option<HBITMAP> {
    let (base_card, _) = card_layout(monitor);
    let (base_w, base_h) = (base_card.right - base_card.left, base_card.bottom - base_card.top);
    let width = rect.right - rect.left;
    let height = rect.bottom - rect.top;
    if width != base_w || height != base_h {
        // The card's own surface is always created at its natural
        // (unscrolled) size — see `rebuild_cards` — so this path only
        // triggers if a card's rect and the reference card_layout ever
        // disagree, which would itself be a bug; fail safe (flat fill)
        // rather than stretch here, since GPU content is drawn once at
        // natural size, not per-frame like the old GDI zoom path.
        return None;
    }
    scaled_wallpaper(base_w, base_h).map(|h| HBITMAP(h as *mut c_void))
}
```

- [ ] **Step 3: Wire into `build_carousel_pages` and `rebuild_open_overview_pages`**

Both functions already loop `for (page, &ws_id) in workspace_ids.iter().enumerate()` (build) / call `build_carousel_pages` then swap the mode (rebuild). After `build_carousel_pages` computes its final `cards`/`thumbs` (right before its `Ok`-shaped return in both `open_overview` and `rebuild_open_overview_pages`), add a GPU repaint pass. Concretely, in `open_overview` (right after the `STATE.with` block that sets `ov.mode = OverviewMode::Opening { .. }`, still within a scope that can re-borrow `STATE`) and in `rebuild_open_overview_pages` (right after its `STATE.with` block that sets `ov.mode = OverviewMode::Open { .. }`), add:

```rust
STATE.with(|s| {
    if let Some(state) = s.borrow_mut().as_mut() {
        if let Some(ov) = state.overviews.get_mut(monitor) {
            if let Some(gpu) = ov.gpu.as_mut() {
                super::overview_gpu::rebuild_cards(gpu, ov.hwnd, &cards);
                for card_visual in &gpu.cards {
                    let card = cards.iter().find(|c| c.page == card_visual.page);
                    if let Some(card) = card {
                        let page_thumbs: Vec<&ThumbAnim> =
                            thumbs.iter().filter(|t| t.page == card.page).collect();
                        super::overview_gpu::paint_card(card_visual, card.rect, &page_thumbs, monitor);
                    }
                }
            }
        }
    }
});
```

(`cards`/`thumbs` are already in scope as the locals returned from `build_carousel_pages` at both call sites.)

- [ ] **Step 4: Build and lint**

Run: `cargo build --workspace` — expect clean.
Run: `cargo clippy -p groveshell-ui --no-deps` — expect clean.

- [ ] **Step 5: Manual verification**

Run the app (`cargo run -p groveshell-ui` or the existing launch path), open Activities. Since nothing yet composites the card visuals into a visible position (Task 6 adds `SetTransform2`/`AddVisual` wiring), this step only confirms no crash/panic and that `cargo build`/clippy pass — visual confirmation happens after Task 6.

- [ ] **Step 6: Commit**

```bash
git add apps/ui/src/imp/overview_gpu.rs apps/ui/src/imp/overview.rs
git commit -m "feat: paint overview card content (wallpaper, thumbnails, icons, chips) via Direct2D"
```

---

### Task 6: Parent card visuals under root, apply carousel transform every tick

Cards created in Task 5 aren't yet attached to the root visual, so nothing shows. This task adds them via `AddVisual` at creation/rebuild time, and replaces the carousel-motion `repaint_overview` calls with a `SetTransform2`-per-card update.

**Files:**
- Modify: `apps/ui/src/imp/overview_gpu.rs` (append `attach_cards`, `update_transforms`)
- Modify: `apps/ui/src/imp/overview.rs:1040-1043` (`on_overview_drag_move`), `:1094-1102` (`on_overview_hover`), `:2429-2529` (`on_animation_tick`)

**Interfaces:**
- Consumes: `overview::{displayed_rect, zoom_rect, card_layout}` (existing, unchanged — same math, just consumed differently), `OverviewInstance.{mode, carousel_offset, gpu}`.
- Produces: `pub(crate) fn update_transforms(gpu: &OverviewGpuState, monitor: &str, carousel_offset: f64, zoom: f64)`.

- [ ] **Step 1: Attach each card visual to the root when (re)built**

In `overview_gpu.rs`'s `rebuild_cards`, after computing `rebuilt`, attach every visual to the root (idempotent — `AddVisual` on an already-attached visual is a harmless no-op per DirectComposition's documented behavior, and this only runs on page rebuild, not per tick):

```rust
pub(crate) fn rebuild_cards(state: &mut OverviewGpuState, hwnd: HWND, cards: &[CardAnim]) {
    // ... existing body computing `rebuilt` ...
    state.cards = rebuilt;
    for card_visual in &state.cards {
        // SAFETY: both visuals belong to the same `IDCompositionDesktopDevice`;
        // `root.visual`/`card_visual.surface.visual` are alive for as
        // long as `state` is.
        unsafe {
            let _ = state.root.visual().AddVisual(card_visual.surface.visual(), true, None);
        }
    }
}
```

This requires `GpuSurface` to expose its visual (currently private/`#[allow(dead_code)]`). In `gpu.rs`, remove the `#[allow(dead_code)]` on the `visual` field and add:

```rust
impl GpuSurface {
    pub(crate) fn visual(&self) -> &IDCompositionVisual2 {
        &self.visual
    }
}
```

- [ ] **Step 2: Add the per-tick transform function**

Append to `overview_gpu.rs`:

```rust
use windows::Foundation::Numerics::Matrix3x2;

use super::overview::{card_layout, displayed_rect, zoom_rect};

/// Applies this tick's carousel position/zoom to every card visual —
/// the GPU-path replacement for `repaint_overview`'s full redraw.
/// `card_layout`/`displayed_rect`/`zoom_rect` are the exact same pure
/// functions `paint_overview` already used; only what happens with
/// their result changes (a transform instead of a redraw).
pub(crate) fn update_transforms(gpu: &OverviewGpuState, monitor: &str, carousel_offset: f64, zoom: f64) {
    let (card_rect, pitch) = card_layout(monitor);
    let anchor_x = (card_rect.left + card_rect.right) as f64 / 2.0;
    let anchor_y = (card_rect.top + card_rect.bottom) as f64 / 2.0;
    for card_visual in &gpu.cards {
        // The card's surface was created at its own natural rect's
        // top-left; `displayed_rect`/`zoom_rect` operate in the
        // overview window's coordinate space, so the transform must
        // carry both the translate-to-displayed-position *and* the
        // uniform scale, in that surface-local-origin frame.
        let base = RECT {
            left: card_rect.left,
            top: card_rect.top,
            right: card_rect.right,
            bottom: card_rect.bottom,
        };
        let displayed = displayed_rect(base, card_visual.page, carousel_offset, pitch, card_rect);
        let zoomed = zoom_rect(displayed, anchor_x, anchor_y, zoom);
        let (base_w, base_h) = ((base.right - base.left).max(1) as f32, (base.bottom - base.top).max(1) as f32);
        let scale_x = (zoomed.right - zoomed.left) as f32 / base_w;
        let scale_y = (zoomed.bottom - zoomed.top) as f32 / base_h;
        let matrix = Matrix3x2 {
            M11: scale_x,
            M12: 0.0,
            M21: 0.0,
            M22: scale_y,
            M31: zoomed.left as f32,
            M32: zoomed.top as f32,
        };
        // SAFETY: `card_visual.surface.visual()` is alive for as long
        // as `gpu` is; `SetTransform2` takes a plain matrix pointer,
        // valid for the duration of this synchronous call.
        unsafe {
            let _ = card_visual.surface.visual().SetTransform2(&matrix);
        }
    }
    gpu::commit();
}
```

Add `pub(crate) fn commit()` to `gpu.rs` (a thin wrapper committing the process-wide `dcomp_device`, used whenever a caller only touched visuals/transforms and didn't already go through `redraw`/`set_opacity`, both of which already commit internally):

```rust
pub(crate) fn commit() {
    GPU.with(|g| {
        let g = g.borrow();
        if let Some(ctx) = g.as_ref() {
            // SAFETY: `ctx.dcomp_device` is the process-wide device,
            // alive for the process's life.
            unsafe {
                let _ = ctx.dcomp_device.Commit();
            }
        }
    });
}
```

- [ ] **Step 3: Replace carousel-motion `repaint_overview` calls with the GPU path where available**

In `overview.rs`, each of the three call sites needs the same shape: if `ov.gpu` is `Some`, call `overview_gpu::update_transforms` instead of `repaint_overview`; otherwise (fallback) call `repaint_overview` exactly as today. `on_overview_drag_move` (currently ends with `if let Some(overview_hwnd) = overview_hwnd { repaint_overview(overview_hwnd); }`) becomes:

```rust
if let Some(overview_hwnd) = overview_hwnd {
    let gpu_updated = STATE.with(|s| {
        let state = s.borrow();
        let ov = state.as_ref()?.overviews.get(monitor)?;
        let gpu = ov.gpu.as_ref()?;
        overview_gpu::update_transforms(gpu, monitor, ov.carousel_offset, 1.0);
        Some(())
    });
    if gpu_updated.is_none() {
        repaint_overview(overview_hwnd);
    }
}
```

`on_overview_hover`'s tail (currently `SetTimer(...); repaint_overview(overview_hwnd);`) gets the same `gpu_updated`-gated replacement in place of its `repaint_overview` call (hover doesn't move the carousel, so `carousel_offset`/`zoom` are unchanged, but re-issuing the same transform is harmless and keeps this call site's shape identical to the drag-move one — simpler than special-casing "no-op" here).

`on_animation_tick`'s `repaint_overview(overview_hwnd);` call (near the end, right after the `SetLayeredWindowAttributes` block) becomes the same pattern, using the tick's own `zoom` value. `on_animation_tick` doesn't currently compute `zoom` outside of `paint_overview` — add the same `zoom` computation `paint_overview` already does (mirrored from overview.rs:1574-1584) inline in `on_animation_tick`'s `STATE.with` closure, returned alongside `carousel_offset`, so both are available at the repaint call site without re-borrowing `STATE` a second time. Reading the current `ov.mode` for the `Opening`/`Closing` branch's `t`/`ease_out` is already done in that closure for `fade_alpha` — reuse the same `started`/`progress` values.

- [ ] **Step 4: Build and lint**

Run: `cargo build --workspace` — expect clean.
Run: `cargo clippy -p groveshell-ui --no-deps` — expect clean.

- [ ] **Step 5: Manual verification**

Run the app, open Activities on a GPU-enabled machine. Confirm: cards appear in their correct carousel positions; dragging the carousel and releasing (snap) moves cards smoothly; switching workspaces via keyboard arrows slides correctly. (Root chrome — dock/search/ghost — and open/close fade aren't wired yet; expect those areas blank/unfaded until Tasks 7–8.)

- [ ] **Step 6: Commit**

```bash
git add apps/ui/src/imp/overview_gpu.rs apps/ui/src/imp/overview.rs apps/ui/src/imp/gpu.rs
git commit -m "feat: drive the overview carousel's position/zoom via compositor transforms"
```

---

### Task 7: Root visual opacity for open/close fade

Replace `SetLayeredWindowAttributes` (whole-window alpha) with `SetOpacity2` on the root visual, when GPU is available. Since every card visual is parented under the root (Task 6), fading the root fades the whole tree automatically — no per-card opacity needed.

**Files:**
- Modify: `apps/ui/src/imp/overview.rs:637-646` (`open_overview`'s `SetLayeredWindowAttributes` call), `:2518-2524` (`on_animation_tick`'s fade-alpha block)

**Interfaces:**
- Consumes: `gpu::set_opacity` (Task 1), `OverviewGpuState.root` (Task 3).

- [ ] **Step 1: `open_overview`'s initial (invisible) alpha**

Change:
```rust
unsafe {
    let _ = SetLayeredWindowAttributes(overview_hwnd, COLORREF(0), 0, LWA_ALPHA);
    ...
```
to branch on GPU availability, mirroring the fallback pattern from Task 6:
```rust
let gpu_root_opacity_set = STATE.with(|s| {
    let state = s.borrow();
    let ov = state.as_ref()?.overviews.get(monitor)?;
    let gpu = ov.gpu.as_ref()?;
    gpu::set_opacity(&gpu.root, 0.0);
    Some(())
});
// SAFETY: `overview_hwnd` is a valid, process-lifetime window.
unsafe {
    if gpu_root_opacity_set.is_none() {
        let _ = SetLayeredWindowAttributes(overview_hwnd, COLORREF(0), 0, LWA_ALPHA);
    }
    let _ = ShowWindow(overview_hwnd, SW_SHOW);
    ...
```
(`ShowWindow`/`SetForegroundWindow`/`SetFocus`/`raise_bars_topmost`/`SetTimer` stay unconditional — the window itself must still be shown either way; DirectComposition content on an invisible window simply wouldn't render, same as GDI content wouldn't).

- [ ] **Step 2: `on_animation_tick`'s per-tick fade**

Change:
```rust
if let Some(alpha) = fade_alpha {
    // SAFETY: `overview_hwnd` is a valid, process-lifetime window
    // already created with `WS_EX_LAYERED`.
    unsafe {
        let _ = SetLayeredWindowAttributes(overview_hwnd, COLORREF(0), alpha, LWA_ALPHA);
    }
}
```
to:
```rust
if let Some(alpha) = fade_alpha {
    let gpu_root_opacity_set = STATE.with(|s| {
        let state = s.borrow();
        let ov = state.as_ref()?.overviews.get(monitor)?;
        let gpu = ov.gpu.as_ref()?;
        gpu::set_opacity(&gpu.root, alpha as f32 / 255.0);
        Some(())
    });
    if gpu_root_opacity_set.is_none() {
        // SAFETY: `overview_hwnd` is a valid, process-lifetime window
        // already created with `WS_EX_LAYERED`.
        unsafe {
            let _ = SetLayeredWindowAttributes(overview_hwnd, COLORREF(0), alpha, LWA_ALPHA);
        }
    }
}
```

- [ ] **Step 3: Build and lint**

Run: `cargo build --workspace` — expect clean.
Run: `cargo clippy -p groveshell-ui --no-deps` — expect clean.

- [ ] **Step 4: Manual verification**

Open and close Activities repeatedly on a GPU-enabled machine: confirm the fade in/out is smooth and the cards (from Task 6) fade with it, not independently. Confirm idle CPU while the overview sits open and un-animated is near-zero (Task Manager), matching the calendar's already-verified behavior.

- [ ] **Step 5: Commit**

```bash
git add apps/ui/src/imp/overview.rs
git commit -m "feat: drive overview open/close fade via compositor opacity instead of layered-window alpha"
```

---

### Task 8: Root content — dock, search panel, and the drag ghost

Port the remaining GDI drawing (dock bar/icons/running-dots, search panel/rows/text, the dragged-window ghost) to Direct2D, targeting the root surface. Wired at the same event-driven call sites that already trigger a `repaint_overview` for these specifically (dock/search/drag state changes) — never on the per-tick carousel path.

**Files:**
- Modify: `apps/ui/src/imp/overview_gpu.rs` (append `paint_root`)
- Modify: `apps/ui/src/imp/overview.rs` (call `paint_root` at the existing dock/search/drag-state-change `repaint_overview` sites: `on_overview_click`'s search-cancel branch, `on_overview_drag_start`, `on_overview_drag_move`'s window-drag branch, `on_overview_hover`, `on_window_drop`, `on_overview_char`, `on_animation_tick`'s per-tick call)

**Interfaces:**
- Consumes: `overview::{dock::dock_layout, DockApp, search_results, search_layout}` (existing), `gpu::{fill_rounded_rect, draw_text, bitmap_from_hbitmap, draw_rounded_bitmap, redraw}` (Tasks 1–2).
- Produces: `pub(crate) fn paint_root(gpu: &OverviewGpuState, monitor: &str, ov: &super::overview::OverviewInstance)`.

- [ ] **Step 1: Write `paint_root`**

Append to `overview_gpu.rs`, mirroring the dock/search/ghost slice of `paint_overview` (overview.rs:1687-1699 for dock data, :1917-1987 for search panel, :1989-2025 for the ghost), reusing `paint_card`'s `rect_to_d2d`/`icon_to_hbitmap` helpers:

```rust
/// Paints the root surface's chrome: the dock bar/icons/running-dots,
/// the search panel/rows, and the dragged-window ghost. Mirrors the
/// corresponding slice of `overview::paint_overview`'s GDI drawing.
/// Triggered only by the same state changes that already invalidated
/// this content under GDI (dock/search/drag changes) — never by the
/// per-tick carousel-position path (see `update_transforms`).
pub(crate) fn paint_root(gpu: &OverviewGpuState, monitor: &str, ov: &super::overview::OverviewInstance) {
    let dpi = super::state::reference_dpi();
    let dock_radius = scaled(super::dock::DOCK_CORNER_RADIUS, dpi) as f32;
    let thumb_radius = scaled(8, dpi) as f32;

    gpu::redraw(&gpu.root, |ctx: &ID2D1DeviceContext| {
        let (dock_bar_rect, dock_slots) = super::dock::dock_layout(monitor, ov.dock_apps.len());
        if !ov.dock_apps.is_empty() {
            let bar = rect_to_d2d(dock_bar_rect, 0, 0);
            gpu::fill_rounded_rect(ctx, bar, dock_radius, 0x002A2A2A);
            for (app, slot) in ov.dock_apps.iter().zip(dock_slots.iter()) {
                let Some(icon) = app.icon else { continue };
                let size = (slot.right - slot.left).max(1);
                if let Some(icon_bitmap) = icon_to_hbitmap(icon, size) {
                    if let Some(bitmap) = gpu::bitmap_from_hbitmap(ctx, icon_bitmap) {
                        gpu::draw_rounded_bitmap(ctx, rect_to_d2d(*slot, 0, 0), 0.0, &bitmap);
                    }
                    // SAFETY: created locally above, owned exclusively here.
                    unsafe {
                        let _ = DeleteObject(icon_bitmap);
                    }
                }
            }
        }

        let results = if ov.search_query.is_empty() {
            Vec::new()
        } else {
            super::overview::search_results(monitor, &ov.search_query)
        };
        let (panel, rows) = super::overview::search_layout(monitor, dpi, results.len());
        gpu::fill_rounded_rect(ctx, rect_to_d2d(panel, 0, 0), thumb_radius, 0x002A2A2A);
        let header_text = if ov.search_query.is_empty() {
            "Type to search".to_string()
        } else {
            format!("Search: {}", ov.search_query)
        };
        gpu::draw_text(ctx, rect_to_d2d(rows[0], 0, 0), &header_text, 0x00A0A0A0, 14.0, false);
        for (i, result) in results.iter().enumerate() {
            let label = match result {
                super::overview::SearchResult::Window { title, .. } => title.clone(),
                super::overview::SearchResult::App { name, .. } => format!("{name}  (launch)"),
            };
            gpu::draw_text(ctx, rect_to_d2d(rows[i + 1], 0, 0), &label, 0x00E0E0E0, 14.0, false);
        }

        if let Some(drag) = ov.window_drag.as_ref() {
            let (base_w, base_h) = (drag.base_w, drag.base_h);
            if let Some(scaled_handle) = super::overview::slot_scaled_snapshot(drag.hwnd, base_w, base_h) {
                let gw = base_w * 3 / 5;
                let gh = base_h * 3 / 5;
                let rect = D2D_RECT_F {
                    left: (drag.cur_x - gw / 2) as f32,
                    top: (drag.cur_y - gh / 2) as f32,
                    right: (drag.cur_x + gw / 2) as f32,
                    bottom: (drag.cur_y + gh / 2) as f32,
                };
                if let Some(bitmap) =
                    gpu::bitmap_from_hbitmap(ctx, HBITMAP(scaled_handle as *mut c_void))
                {
                    gpu::draw_rounded_bitmap(ctx, rect, thumb_radius, &bitmap);
                }
            }
        }
    });
}
```

Note: `SearchResult` needs `pub(crate)` visibility (currently private to `overview.rs` per its `enum SearchResult` declaration at overview.rs:282) — widen it to `pub(crate) enum SearchResult` and its two variants' fields to `pub(crate)`, since this is now read from `overview_gpu.rs` too. Likewise `search_results`/`search_layout` need `pub(crate)` (currently private `fn`).

- [ ] **Step 2: Wire the call sites**

Every place `overview.rs` already calls `repaint_overview(overview_hwnd)` specifically because dock/search/drag state changed (not carousel position) gets a GPU-branch added identically to Task 6/7's pattern:

```rust
let gpu_repainted = STATE.with(|s| {
    let state = s.borrow();
    let ov = state.as_ref()?.overviews.get(monitor)?;
    let gpu = ov.gpu.as_ref()?;
    overview_gpu::paint_root(gpu, monitor, ov);
    Some(())
});
if gpu_repainted.is_none() {
    repaint_overview(overview_hwnd);
}
```

Apply this replacement at: `on_overview_click`'s search-cancel branch (overview.rs:793-798), `on_overview_drag_start`'s tail (no existing repaint call there today, since `SetTimer` alone drives the next tick's repaint via `on_animation_tick` — no change needed there), `on_overview_drag_move`'s window-drag branch (the `Some(ov.hwnd)` return path feeds into the same tail as the carousel-drag branch — merge with Task 6's replacement so both branches funnel through one `gpu_updated`/`gpu_repainted`-gated block that calls both `update_transforms` *and* `paint_root` when a window (not carousel) drag is active), `on_overview_hover`'s tail (already covered by Task 6's replacement — extend it to also call `paint_root`, since hover changes dock/thumb glow state that needs root+card repaints together... note: thumb hover glow is drawn as part of card content per Task 5's scope; extending `paint_card` for the hovered card, not `paint_root`, is the correct target — see Step 3), `on_window_drop`'s tail, `on_overview_char`'s `Action::Repaint`/`Action::ActivateFirst` arms.

- [ ] **Step 3: Hover-glow repaint scoping**

`thumb_hover_glow`/`hover_glow` (card-level and per-thumbnail glow) belong on card content, not root — per the design's scope ("redrawn only by ... hover-glow state changes ... targeting that one card's small surface"). Add a `stroke_rounded_rect` call to `paint_card` (Task 5) using `stroke_rounded_rect(ctx, full, card_radius, 0x0060A8FF, intensity, 2.0)` when the card being painted matches `ov.window_drag.as_ref().and_then(|d| d.hover_page)` or a thumbnail's rect strokes similarly when it matches `ov.hover_thumb`. Since `paint_card` doesn't currently take `ov`, thread an additional `hover: Option<(HoverTarget, f64)>` parameter through it — a small signature addition, not a new file. Wire `on_overview_hover`'s tail to re-run `paint_card` for whichever card is affected (looked up via `gpu.cards.iter().find(|cv| cv.page == page)`) instead of `paint_root`.

- [ ] **Step 4: Build and lint**

Run: `cargo build --workspace` — expect clean.
Run: `cargo clippy -p groveshell-ui --no-deps` — expect clean.

- [ ] **Step 5: Manual verification**

With the app running: dock icons show and launch/focus correctly; typing while Activities is open shows live search results; dragging a window preview between cards shows the ghost and drops correctly; hovering a card while dragging (and plain hover over a thumbnail) shows the glow. Confirm idle CPU stays near-zero when nothing is actively animating or being interacted with.

- [ ] **Step 6: Commit**

```bash
git add apps/ui/src/imp/overview_gpu.rs apps/ui/src/imp/overview.rs
git commit -m "feat: paint the overview's dock, search panel, and drag ghost via Direct2D"
```

---

### Task 9: Per-monitor GDI fallback verification

Confirm the fallback path (this specific overview window's DirectComposition setup failing, even though the process-wide device succeeded) still produces a fully working overview — the exact same guarantee already proven for the calendar in Phase 1.

**Files:**
- No production code changes expected; this task is verification-only, following the same convention as the calendar phase's final task.

- [ ] **Step 1: Force the failure path**

Temporarily edit `overview_gpu::create` to always return `None` (e.g. `pub(crate) fn create(_: HWND, _: i32, _: i32) -> Option<OverviewGpuState> { None }`), rebuild, and run the app.

- [ ] **Step 2: Manual verification**

Confirm the overview behaves exactly as it did before this entire plan: full-scene GDI redraws, `SetLayeredWindowAttributes` fade, all interactions (drag, hover, dock, search) working — since every call site added in Tasks 5–8 falls back to its pre-existing `repaint_overview`/`SetLayeredWindowAttributes` call whenever `ov.gpu` is `None`.

- [ ] **Step 3: Revert the forced failure**

```bash
git diff apps/ui/src/imp/overview_gpu.rs
git checkout -- apps/ui/src/imp/overview_gpu.rs
```
(Only if Step 1's edit was made directly on top of the real implementation without committing — confirm `git status` shows no unintended staged changes before continuing.)

- [ ] **Step 4: Final full-branch check**

Run: `cargo build --workspace`
Run: `cargo test --workspace`
Run: `cargo clippy --workspace --no-deps`

Expect all clean, matching the state before Step 1's temporary edit.

---

### Self-Review Notes

- **Spec coverage:** Visual tree (§1) → Tasks 3–4; per-tick transform/opacity (§2) → Tasks 6–7; WIC bridge (§3) → Task 2 (`bitmap_from_hbitmap`) used throughout Tasks 5/8; per-monitor fallback (§4) → Task 9; Testing → every task's build/clippy/manual-verification steps, consistent with the design doc's stated testing convention (pure geometry needs no new tests; compositor behavior is manual-only).
- **Type consistency:** `OverviewGpuState`/`CardVisual`/`create`/`rebuild_cards`/`paint_card`/`paint_root`/`update_transforms` are used with the same names and signatures introduced in Task 3 throughout every later task.
- **Scope:** drag-pop/hover-glow/search/dock rendering are ported (drawing logic moved to Direct2D, per Global Constraints) but not further optimized or made compositor-animated, consistent with the design's explicit non-goals.
