# DirectComposition Rendering Foundation (Phase 1) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Prove out a DirectComposition/Direct2D rendering pipeline — GPU-composited, not app-redrawn — on the calendar flyout, with a GDI fallback if GPU setup fails, decided once at process startup.

**Architecture:** A new `gpu` module owns process-wide D3D11/Direct2D/DirectComposition/DirectWrite device setup and a small per-window surface type. `calendar.rs`'s rendering is ported to draw through this module instead of raw GDI; the GDI implementation stays in the file, completely unchanged, as the fallback path.

**Tech Stack:** Rust, `windows` crate (Win32 Direct2D/Direct3D11/DirectComposition/DirectWrite bindings) — no new external crates.

## Global Constraints

- The GPU-available decision is made exactly once, at process startup, before any window is created — never retried, never re-evaluated per window or per calendar-open.
- If GPU setup fails (process-wide or per-window), the affected window keeps using its existing GDI painting completely unchanged — the fallback is "don't touch working code," not a parallel reimplementation.
- `WS_EX_NOREDIRECTIONBITMAP` is deliberately **not** added to the calendar window's style in this phase: it would break the GDI fallback if per-window target creation fails after window creation, and the redirection-bitmap overhead is negligible for a small flyout. This is a documented simplification, not an oversight.
- Only what `paint_calendar` needs today gets a Direct2D equivalent — no general drawing engine, no features calendar doesn't already use.
- Per the design's testing section: pure logic gets unit tests; live GPU/compositor behavior is manual-verification-only, consistent with this codebase's established convention for Win32-integration work.

---

### Task 1: Unit tests for the calendar's pure date math

**Files:**
- Modify: `apps/ui/src/imp/calendar.rs` (add a test module at the end)

**Interfaces:** none — this task adds tests for functions that already exist (`is_leap_year`, `days_in_month`, `month_name`), unrelated to the GPU work. Placed first because it has no dependencies on anything else in this plan.

- [ ] **Step 1: Write the tests**

Add to the end of `apps/ui/src/imp/calendar.rs`:

```rust
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
```

- [ ] **Step 2: Run the tests**

Run: `cargo test -p groveshell-ui calendar::tests`
Expected: PASS, 4/4 (these test existing, already-correct functions — there's no RED step here, just confirming coverage).

- [ ] **Step 3: Commit**

```bash
git add apps/ui/src/imp/calendar.rs
git commit -m "test: add unit tests for the calendar's date-grid math"
```

---

### Task 2: Enable the `windows` crate features this plan needs

**Files:**
- Modify: `apps/ui/Cargo.toml`

**Interfaces:** none — this just makes the APIs used by later tasks available to compile against.

- [ ] **Step 1: Add the features**

In `apps/ui/Cargo.toml`, add to the `windows` dependency's `features` list (alphabetically among the existing entries):

```toml
  "Foundation_Numerics",
  "Win32_Graphics_Direct2D",
  "Win32_Graphics_Direct2D_Common",
  "Win32_Graphics_Direct3D",
  "Win32_Graphics_Direct3D11",
  "Win32_Graphics_DirectComposition",
  "Win32_Graphics_DirectWrite",
  "Win32_Graphics_Dxgi",
  "Win32_Graphics_Dxgi_Common",
```

- [ ] **Step 2: Confirm it builds**

Run: `cargo build -p groveshell-ui 2>&1 | tail -30`
Expected: clean — no code uses these features yet, so this is just confirming the feature names resolve and pull in their dependent Windows metadata crates without conflict.

- [ ] **Step 3: Commit**

```bash
git add apps/ui/Cargo.toml
git commit -m "build: enable Direct2D/Direct3D11/DirectComposition/DirectWrite windows-rs features"
```

---

### Task 3: Process-wide GPU device setup

**Files:**
- Create: `apps/ui/src/imp/gpu.rs`
- Modify: `apps/ui/src/imp/mod.rs` (register `mod gpu;`, call `gpu::init()` once at startup)

**Interfaces:**
- Produces: `pub(crate) fn init()` — sets up the process-wide devices; call once, before any window is created. `pub(crate) fn is_enabled() -> bool` — whether setup succeeded; every later GPU-path call site checks this.

- [ ] **Step 1: Create `apps/ui/src/imp/gpu.rs`**

```rust
//! DirectComposition/Direct2D rendering foundation (Phase 1 of the
//! animation-fluidity work — see
//! `docs/superpowers/specs/2026-07-29-directcomposition-rendering-foundation-design.md`).
//! Owns process-wide GPU device setup and per-window composition
//! surfaces. If setup fails at any point, `is_enabled()` returns `false`
//! for the rest of the process's life and every caller falls back to its
//! existing GDI painting, unchanged.

use std::cell::RefCell;

use windows::core::Interface;
use windows::Win32::Graphics::Direct2D::{
    D2D1CreateFactory, ID2D1Device, ID2D1Factory1, D2D1_FACTORY_TYPE_SINGLE_THREADED,
};
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION,
};
use windows::Win32::Graphics::DirectComposition::{DCompositionCreateDevice2, IDCompositionDevice};
use windows::Win32::Graphics::DirectWrite::{DWriteCreateFactory, IDWriteFactory, DWRITE_FACTORY_TYPE_SHARED};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;

pub(crate) struct GpuContext {
    pub(crate) dcomp_device: IDCompositionDevice,
    pub(crate) dwrite_factory: IDWriteFactory,
}

thread_local! {
    pub(crate) static GPU: RefCell<Option<GpuContext>> = const { RefCell::new(None) };
}

/// Sets up the process-wide D3D11/DirectComposition/DirectWrite devices,
/// once. Must be called before any GPU-rendered window is created. The
/// decision (GPU available or not) is made exactly here and never
/// revisited — every later caller just checks [`is_enabled`].
pub(crate) fn init() {
    match try_init() {
        Ok(ctx) => GPU.with(|g| *g.borrow_mut() = Some(ctx)),
        Err(e) => {
            tracing::warn!(error = ?e, "GPU rendering setup failed, falling back to GDI for the whole process");
        }
    }
}

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

        let dcomp_device: IDCompositionDevice = DCompositionCreateDevice2(&d2d_device)?;
        let dwrite_factory: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;

        Ok(GpuContext { dcomp_device, dwrite_factory })
    }
}

/// Whether the process-wide GPU setup succeeded. Decided once, at
/// startup, by [`init`] — never re-checked or retried afterwards.
pub(crate) fn is_enabled() -> bool {
    GPU.with(|g| g.borrow().is_some())
}
```

- [ ] **Step 2: Register the module and call `init()` at startup**

In `apps/ui/src/imp/mod.rs`, add `mod gpu;` to the `mod` list (alphabetically after `mod dock;` and before `mod hotplug;`).

Then, in `main()`, right after the existing `GdiplusStartup` call and before the big `unsafe { ... register_class ... }` block that follows it:

```rust
    let _ = unsafe { GdiplusStartup(&mut gdiplus_token, &gdiplus_input, std::ptr::null_mut()) };

    // Decided once, here, before any window exists — see gpu.rs. Every
    // later GPU-path call site just checks `gpu::is_enabled()`; this is
    // never retried or re-evaluated.
    gpu::init();

    // SAFETY: every Win32 call below either has a call-site safety
```

- [ ] **Step 3: Run the build**

Run: `cargo build -p groveshell-ui 2>&1 | tail -40`
Expected: clean.

- [ ] **Step 4: Manual verification**

Run `.\scripts\dev-start.ps1`. Check the log output (per `groveshell_common::logging::init`'s log file, see `docs/PROJECT_PLAN.md` for its location) for either silence (success — nothing to report) or a `"GPU rendering setup failed"` warning if it didn't succeed on this machine. On ordinary Windows 10/11 hardware, expect success. This step has no automated coverage — it's confirming live device creation actually completes on a real machine, not just that the code compiles.

- [ ] **Step 5: Commit**

```bash
git add apps/ui/src/imp/gpu.rs apps/ui/src/imp/mod.rs
git commit -m "feat: process-wide DirectComposition/Direct2D device setup, decided once at startup"
```

---

### Task 4: Per-window GPU surface for the calendar flyout

**Files:**
- Modify: `apps/ui/src/imp/gpu.rs` (add `GpuSurface` and `create_surface`)
- Modify: `apps/ui/src/imp/state.rs` (add `calendar_gpu: Option<gpu::GpuSurface>` to `AppState`)
- Modify: `apps/ui/src/imp/mod.rs` (create the surface right after `calendar_hwnd`, add the field to `AppState`'s construction)

**Interfaces:**
- Consumes: `gpu::GpuContext`, `gpu::GPU` (Task 3)
- Produces: `pub(crate) struct GpuSurface` (opaque outside `gpu.rs` — no public fields), `pub(crate) fn create_surface(hwnd: HWND, width: i32, height: i32) -> Option<GpuSurface>`

- [ ] **Step 1: Add `GpuSurface` and `create_surface` to `gpu.rs`**

Add to `apps/ui/src/imp/gpu.rs`:

```rust
use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::DirectComposition::{IDCompositionSurface, IDCompositionTarget, IDCompositionVisual};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_ALPHA_MODE_PREMULTIPLIED, DXGI_FORMAT_B8G8R8A8_UNORM};

/// One window's GPU-composited surface: a target bound to its `HWND`, a
/// root visual covering the client area, and the surface Direct2D draws
/// into. Fields are private — every window holding a `GpuSurface` only
/// ever passes it back into this module's own functions.
pub(crate) struct GpuSurface {
    #[allow(dead_code)] // kept alive for as long as the surface must render; never read directly
    target: IDCompositionTarget,
    #[allow(dead_code)]
    visual: IDCompositionVisual,
    surface: IDCompositionSurface,
}

/// Creates a `GpuSurface` for `hwnd`, sized `width`×`height`. Returns
/// `None` (never panics) if the process-wide GPU setup isn't available,
/// or if this specific window's setup fails even though the process-wide
/// setup succeeded — both cases mean the caller should keep using its
/// existing GDI painting for this window, unchanged.
pub(crate) fn create_surface(hwnd: HWND, width: i32, height: i32) -> Option<GpuSurface> {
    GPU.with(|g| {
        let g = g.borrow();
        let ctx = g.as_ref()?;
        try_create_surface(ctx, hwnd, width, height).ok()
    })
}

fn try_create_surface(ctx: &GpuContext, hwnd: HWND, width: i32, height: i32) -> windows::core::Result<GpuSurface> {
    // SAFETY: `hwnd` is a valid, process-lifetime window; the device
    // calls below have no other preconditions.
    unsafe {
        let target = ctx.dcomp_device.CreateTargetForHwnd(hwnd, true)?;
        let visual = ctx.dcomp_device.CreateVisual()?;
        let surface = ctx.dcomp_device.CreateSurface(
            width as u32,
            height as u32,
            DXGI_FORMAT_B8G8R8A8_UNORM,
            DXGI_ALPHA_MODE_PREMULTIPLIED,
        )?;
        visual.SetContent(&surface)?;
        target.SetRoot(&visual)?;
        ctx.dcomp_device.Commit()?;
        Ok(GpuSurface { target, visual, surface })
    }
}
```

- [ ] **Step 2: Add the field to `AppState`**

In `apps/ui/src/imp/state.rs`, add near the top:

```rust
use super::gpu::GpuSurface;
```

Add a field to `AppState` (near `calendar_hwnd`/`calendar_open`):

```rust
    /// `None` if GPU rendering isn't available (see `gpu::is_enabled`) —
    /// `paint_calendar`/`toggle_calendar` fall back to plain GDI in that
    /// case, unchanged from before this feature existed.
    pub(crate) calendar_gpu: Option<GpuSurface>,
```

- [ ] **Step 3: Create the surface and populate the field in `main()`**

In `apps/ui/src/imp/mod.rs`, right after `calendar_hwnd` is created (look for the `CreateWindowExW` call using `w!("GroveShellCalendar")`), add:

```rust
        let calendar_gpu = gpu::create_surface(calendar_hwnd, CAL_WIDTH, CAL_HEIGHT);
```

Then add `calendar_gpu,` to the `AppState { ... }` construction, alongside the existing `calendar_hwnd,` field.

- [ ] **Step 4: Run the build**

Run: `cargo build -p groveshell-ui 2>&1 | tail -40`
Expected: clean.

- [ ] **Step 5: Run the existing test suite**

Run: `cargo test -p groveshell-ui`
Expected: all existing tests pass, including Task 1's new calendar date-math tests.

- [ ] **Step 6: Commit**

```bash
git add apps/ui/src/imp/gpu.rs apps/ui/src/imp/state.rs apps/ui/src/imp/mod.rs
git commit -m "feat: create a GPU composition surface for the calendar flyout at startup"
```

---

### Task 5: Direct2D drawing wrapper

**Files:**
- Modify: `apps/ui/src/imp/gpu.rs` (add `redraw`, `fill_rect`, `draw_text`)

**Interfaces:**
- Consumes: `gpu::GpuSurface`, `gpu::GPU` (Tasks 3-4)
- Produces: `pub(crate) fn redraw<F: FnOnce(&ID2D1DeviceContext)>(surface: &GpuSurface, draw: F)`, `pub(crate) fn fill_rect(ctx: &ID2D1DeviceContext, rect: D2D_RECT_F, colorref: u32)`, `pub(crate) fn draw_text(ctx: &ID2D1DeviceContext, rect: D2D_RECT_F, text: &str, colorref: u32, size: f32, center_horizontally: bool)`

- [ ] **Step 1: Add the wrapper functions to `gpu.rs`**

Add to `apps/ui/src/imp/gpu.rs`:

```rust
use windows::Win32::Foundation::POINT;
use windows::Foundation::Numerics::Matrix3x2;
use windows::Win32::Graphics::Direct2D::Common::{D2D1_COLOR_F, D2D_RECT_F};
use windows::Win32::Graphics::Direct2D::{ID2D1DeviceContext, D2D1_DRAW_TEXT_OPTIONS_NONE};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT_NORMAL,
    DWRITE_MEASURING_MODE_NATURAL, DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_CENTER,
    DWRITE_TEXT_ALIGNMENT_LEADING,
};

/// Draws into `surface` via `draw`, then commits so the compositor picks
/// up the new content. `draw` receives a device context whose origin is
/// already adjusted for `BeginDraw`'s update offset, so callers can draw
/// as if the surface's own top-left were always `(0, 0)`.
pub(crate) fn redraw<F>(surface: &GpuSurface, draw: F)
where
    F: FnOnce(&ID2D1DeviceContext),
{
    GPU.with(|g| {
        let g = g.borrow();
        let Some(ctx) = g.as_ref() else { return };
        // SAFETY: `surface.surface` was created by this module's own
        // `try_create_surface` and is still owned (alive) by the
        // caller's `GpuSurface`; `BeginDraw`/`EndDraw` bracket every
        // Direct2D call made inside `draw`, matching Direct2D's required
        // usage pattern.
        unsafe {
            let mut offset = POINT::default();
            let Ok(d2d_ctx): windows::core::Result<ID2D1DeviceContext> =
                surface.surface.BeginDraw(None, &mut offset)
            else {
                return;
            };
            let translate = Matrix3x2 {
                M11: 1.0,
                M12: 0.0,
                M21: 0.0,
                M22: 1.0,
                M31: offset.x as f32,
                M32: offset.y as f32,
            };
            let _ = d2d_ctx.SetTransform(&translate);
            draw(&d2d_ctx);
            let _ = surface.surface.EndDraw();
            let _ = ctx.dcomp_device.Commit();
        }
    });
}

pub(crate) fn fill_rect(ctx: &ID2D1DeviceContext, rect: D2D_RECT_F, colorref: u32) {
    // SAFETY: `ctx` is a live device context between `BeginDraw`/`EndDraw`
    // (enforced by `redraw`'s closure scope).
    unsafe {
        if let Ok(brush) = ctx.CreateSolidColorBrush(&colorref_to_d2d(colorref), None) {
            let _ = ctx.FillRectangle(&rect, &brush);
        }
    }
}

pub(crate) fn draw_text(
    ctx: &ID2D1DeviceContext,
    rect: D2D_RECT_F,
    text: &str,
    colorref: u32,
    size: f32,
    center_horizontally: bool,
) {
    GPU.with(|g| {
        let g = g.borrow();
        let Some(gpu_ctx) = g.as_ref() else { return };
        // SAFETY: same as `fill_rect`; `gpu_ctx.dwrite_factory` is the
        // process-wide factory from `init`, alive for the process's life.
        unsafe {
            let Ok(format) = gpu_ctx.dwrite_factory.CreateTextFormat(
                windows::core::w!("Segoe UI"),
                None,
                DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                size,
                windows::core::w!("en-us"),
            ) else {
                return;
            };
            let _ = format.SetTextAlignment(if center_horizontally {
                DWRITE_TEXT_ALIGNMENT_CENTER
            } else {
                DWRITE_TEXT_ALIGNMENT_LEADING
            });
            let _ = format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);

            let Ok(brush) = ctx.CreateSolidColorBrush(&colorref_to_d2d(colorref), None) else {
                return;
            };
            let wide: Vec<u16> = text.encode_utf16().collect();
            let _ = ctx.DrawText(
                &wide,
                &format,
                &rect,
                &brush,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
                DWRITE_MEASURING_MODE_NATURAL,
            );
        }
    });
}

/// `COLORREF` is `0x00BBGGRR` — the same bit layout every GDI color
/// constant in this codebase already uses (e.g. `calendar.rs`'s
/// `COLORREF(0x00A0A0A0)`), so callers can pass those exact literals
/// unchanged.
fn colorref_to_d2d(colorref: u32) -> D2D1_COLOR_F {
    D2D1_COLOR_F {
        r: (colorref & 0xFF) as f32 / 255.0,
        g: ((colorref >> 8) & 0xFF) as f32 / 255.0,
        b: ((colorref >> 16) & 0xFF) as f32 / 255.0,
        a: 1.0,
    }
}
```

Add `"Foundation_Numerics"` to `apps/ui/Cargo.toml`'s `windows` feature list if Task 2 didn't already include it (it does — this is just confirming the same feature list from Task 2 covers `windows::Foundation::Numerics::Matrix3x2`).

- [ ] **Step 2: Run the build**

Run: `cargo build -p groveshell-ui 2>&1 | tail -40`
Expected: clean (these functions are unused so far — Task 6 wires them up — a `never used` warning is expected and fine at this point, resolved by Task 6).

- [ ] **Step 3: Commit**

```bash
git add apps/ui/src/imp/gpu.rs apps/ui/Cargo.toml
git commit -m "feat: add a minimal Direct2D drawing wrapper (fill_rect, draw_text, redraw)"
```

---

### Task 6: Port `paint_calendar` to the GPU path

**Files:**
- Modify: `apps/ui/src/imp/calendar.rs` (`paint_calendar`, `toggle_calendar`)

**Interfaces:**
- Consumes: `gpu::is_enabled`, `gpu::redraw`, `gpu::fill_rect`, `gpu::draw_text` (Task 5), `AppState.calendar_gpu` (Task 4)

- [ ] **Step 1: Add the GPU-path content function**

Add to `apps/ui/src/imp/calendar.rs` (near `paint_calendar`):

```rust
/// Draws the calendar's content (background, header, day grid,
/// notifications section) through the Direct2D wrapper — the GPU-path
/// equivalent of the GDI drawing `paint_calendar` does below. Every
/// coordinate and color here is copied from `paint_calendar` unchanged;
/// this must stay a faithful port, not a redesign.
fn paint_calendar_content(ctx: &windows::Win32::Graphics::Direct2D::ID2D1DeviceContext) {
    use windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F;

    // Background fill: replicates the `GroveShellCalendar` window
    // class's solid brush (`COLORREF(0x00303030)`), which `WM_ERASEBKGND`
    // painted automatically before GDI's `paint_calendar` ran. Once this
    // surface owns the window's visual content, nothing else provides
    // that background any more, so it has to be drawn explicitly here.
    super::gpu::fill_rect(
        ctx,
        D2D_RECT_F { left: 0.0, top: 0.0, right: CAL_WIDTH as f32, bottom: CAL_HEIGHT as f32 },
        0x00303030,
    );

    // SAFETY: plain query, no preconditions.
    let now = unsafe { GetLocalTime() };
    let year = now.wYear as i32;
    let month = now.wMonth as i32;
    let today = now.wDay as i32;
    let today_dow = now.wDayOfWeek as i32;
    let first_dow = ((today_dow - (today - 1)) % 7 + 7) % 7;
    let days = days_in_month(year, month);

    super::gpu::draw_text(
        ctx,
        D2D_RECT_F { left: CAL_PADDING as f32, top: 8.0, right: (CAL_WIDTH - CAL_PADDING) as f32, bottom: 32.0 },
        &format!("{} {year}", month_name(month)),
        0x00FFFFFF,
        16.0,
        true,
    );

    let cell_w = (CAL_WIDTH - CAL_PADDING * 2) / 7;
    const DOW_LABELS: [&str; 7] = ["Su", "Mo", "Tu", "We", "Th", "Fr", "Sa"];
    for (i, label) in DOW_LABELS.iter().enumerate() {
        let x = CAL_PADDING + i as i32 * cell_w;
        super::gpu::draw_text(
            ctx,
            D2D_RECT_F { left: x as f32, top: 40.0, right: (x + cell_w) as f32, bottom: 60.0 },
            label,
            0x00A0A0A0,
            12.0,
            true,
        );
    }

    let mut day = 1;
    let mut col = first_dow;
    let mut row = 0;
    while day <= days {
        let x = CAL_PADDING + col * cell_w;
        let y = 64 + row * CAL_CELL_HEIGHT;
        let color = if day == today { 0x0040A0FF } else { 0x00E0E0E0 };
        super::gpu::draw_text(
            ctx,
            D2D_RECT_F {
                left: x as f32,
                top: y as f32,
                right: (x + cell_w) as f32,
                bottom: (y + CAL_CELL_HEIGHT) as f32,
            },
            &day.to_string(),
            color,
            14.0,
            true,
        );
        day += 1;
        col += 1;
        if col == 7 {
            col = 0;
            row += 1;
        }
    }

    // Notifications section: left-aligned (`center_horizontally: false`),
    // matching `paint_calendar`'s `DT_SINGLELINE | DT_VCENTER` (no
    // `DT_CENTER`) for these two lines specifically.
    super::gpu::draw_text(
        ctx,
        D2D_RECT_F {
            left: CAL_PADDING as f32,
            top: (CAL_CALENDAR_HEIGHT + 10) as f32,
            right: (CAL_WIDTH - CAL_PADDING) as f32,
            bottom: (CAL_CALENDAR_HEIGHT + 34) as f32,
        },
        "Notifications",
        0x00FFFFFF,
        14.0,
        false,
    );
    super::gpu::draw_text(
        ctx,
        D2D_RECT_F {
            left: CAL_PADDING as f32,
            top: (CAL_CALENDAR_HEIGHT + 40) as f32,
            right: (CAL_WIDTH - CAL_PADDING) as f32,
            bottom: (CAL_CALENDAR_HEIGHT + 64) as f32,
        },
        "No new notifications",
        0x00A0A0A0,
        12.0,
        false,
    );
}
```

- [ ] **Step 2: Make `paint_calendar` branch on the GPU path**

Change the start of `paint_calendar` from:

```rust
pub(crate) fn paint_calendar(hwnd: HWND) {
    // SAFETY: `hwnd` is the window currently processing `WM_PAINT`.
    unsafe {
        let mut ps = PAINTSTRUCT::default();
        let hdc = BeginPaint(hwnd, &mut ps);
        SetBkMode(hdc, TRANSPARENT);
```

to:

```rust
pub(crate) fn paint_calendar(hwnd: HWND) {
    if super::gpu::is_enabled() {
        // Content is already composited independently by
        // DirectComposition — this just acknowledges the paint request.
        // SAFETY: `hwnd` is the window currently processing `WM_PAINT`.
        unsafe {
            let mut ps = PAINTSTRUCT::default();
            let _ = BeginPaint(hwnd, &mut ps);
            let _ = EndPaint(hwnd, &ps);
        }
        return;
    }
    // SAFETY: `hwnd` is the window currently processing `WM_PAINT`.
    unsafe {
        let mut ps = PAINTSTRUCT::default();
        let hdc = BeginPaint(hwnd, &mut ps);
        SetBkMode(hdc, TRANSPARENT);
```

Everything else in `paint_calendar` (the GDI drawing and the final `EndPaint`) stays completely unchanged — it's the fallback path now, reached only when `gpu::is_enabled()` is `false`.

- [ ] **Step 3: Make `toggle_calendar`'s show path branch on the GPU path**

Change:

```rust
    // SAFETY: `hwnd` is a valid, process-lifetime window.
    unsafe {
        let _ = InvalidateRect(hwnd, None, true);
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(hwnd);
    }
}
```

to:

```rust
    if super::gpu::is_enabled() {
        STATE.with(|s| {
            let state = s.borrow();
            let Some(state) = state.as_ref() else { return };
            let Some(surface) = state.calendar_gpu.as_ref() else { return };
            super::gpu::redraw(surface, paint_calendar_content);
        });
    }

    // SAFETY: `hwnd` is a valid, process-lifetime window.
    unsafe {
        if !super::gpu::is_enabled() {
            let _ = InvalidateRect(hwnd, None, true);
        }
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(hwnd);
    }
}
```

- [ ] **Step 4: Run the build**

Run: `cargo build -p groveshell-ui 2>&1 | tail -40`
Expected: clean, no warnings (the Task 5 "never used" warnings should now be gone since every wrapper function is wired up).

- [ ] **Step 5: Run the full test suite**

Run: `cargo test --workspace --no-fail-fast 2>&1 | tail -100`
Expected: all tests pass, including Task 1's new calendar tests. The two pre-existing `groveshell-config` `PermissionDenied` failures are a known environment issue predating this branch — not a regression.

- [ ] **Step 6: Clippy**

Run: `cargo clippy -p groveshell-ui --no-deps 2>&1 | tail -60`
Expected: no new warnings beyond the one pre-existing `sort_by_key` lint already on `main`.

- [ ] **Step 7: Manual verification — GPU path**

Run `.\scripts\dev-start.ps1`. Click the clock to open the calendar. Confirm:
- It looks the same as before this change: correct month/year, day-of-week row, today highlighted in the same color, notifications section left-aligned below the grid.
- It opens/closes via the same click/Escape/outside-click behavior as before — no interaction regression.
- With the calendar open and idle, check Task Manager's CPU column for the GroveShell process: it should be at or near 0%, not busy-looping — this is the actual point of the phase (the compositor is displaying it, not an app-side redraw loop).

- [ ] **Step 8: Manual verification — GDI fallback path**

Temporarily force the fallback to confirm it still works standalone (a real D3D11 failure can't be reliably forced on ordinary dev hardware): in `gpu::init`, temporarily change the `match try_init() { ... }` to always take the `Err` branch (e.g. `match Err::<GpuContext, _>(windows::core::Error::empty()) { ... }` or simply comment out the `Ok` arm's assignment), rebuild, and confirm the calendar still opens, closes, and looks correct — this is `paint_calendar`'s original GDI code path, now reached via `gpu::is_enabled() == false`. Revert the temporary change afterward; do not commit it.

- [ ] **Step 9: Commit**

```bash
git add apps/ui/src/imp/calendar.rs
git commit -m "feat: port the calendar flyout's rendering to DirectComposition/Direct2D"
```
