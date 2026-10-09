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
use windows::Win32::Graphics::DirectComposition::{DCompositionCreateDevice2, IDCompositionDesktopDevice};
use windows::Win32::Graphics::DirectWrite::{DWriteCreateFactory, IDWriteFactory, DWRITE_FACTORY_TYPE_SHARED};
use windows::Win32::Graphics::Dxgi::IDXGIDevice;

pub struct GpuContext {
    pub dcomp_device: IDCompositionDesktopDevice,
    #[allow(dead_code)] // will be read by overview GPU state in later tasks
    pub d2d_factory: ID2D1Factory1,
    pub dwrite_factory: IDWriteFactory,
}

thread_local! {
    pub static GPU: RefCell<Option<GpuContext>> = const { RefCell::new(None) };
}

/// Sets up the process-wide D3D11/DirectComposition/DirectWrite devices,
/// once. Must be called before any GPU-rendered window is created. The
/// decision (GPU available or not) is made exactly here and never
/// revisited — every later caller just checks [`is_enabled`].
pub fn init() {
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

        let dcomp_device: IDCompositionDesktopDevice = DCompositionCreateDevice2(&d2d_device)?;
        let dwrite_factory: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;

        Ok(GpuContext { dcomp_device, d2d_factory, dwrite_factory })
    }
}

/// Whether the process-wide GPU setup succeeded. Decided once, at
/// startup, by [`init`] — never re-checked or retried afterwards.
pub fn is_enabled() -> bool {
    GPU.with(|g| g.borrow().is_some())
}

use windows::Win32::Foundation::HWND;
use windows::Win32::Graphics::DirectComposition::{IDCompositionSurface, IDCompositionTarget, IDCompositionVisual2, IDCompositionVisual3};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_ALPHA_MODE_PREMULTIPLIED, DXGI_FORMAT_B8G8R8A8_UNORM};

/// One window's GPU-composited surface: a target bound to its `HWND`, a
/// root visual covering the client area, and the surface Direct2D draws
/// into. Fields are private — every window holding a `GpuSurface` only
/// ever passes it back into this module's own functions.
pub struct GpuSurface {
    /// The window's composition target. `Some` only for the *first*
    /// surface created for a window — a window can hold exactly one
    /// target, and asking for a second fails with
    /// `DCOMPOSITION_ERROR_WINDOW_ALREADY_COMPOSED`. Child surfaces
    /// ([`create_child_surface`]) carry `None` and reach the screen by
    /// being attached under that first surface's visual.
    #[allow(dead_code)] // kept alive for as long as the surface must render; never read directly
    target: Option<IDCompositionTarget>,
    visual: IDCompositionVisual2,
    surface: IDCompositionSurface,
    width: i32,
    height: i32,
}

impl GpuSurface {
    pub fn width(&self) -> i32 {
        self.width
    }

    pub fn height(&self) -> i32 {
        self.height
    }

    pub fn visual(&self) -> &IDCompositionVisual2 {
        &self.visual
    }
}

/// Creates a `GpuSurface` for `hwnd`, sized `width`×`height`. Returns
/// `None` (never panics) if the process-wide GPU setup isn't available,
/// or if this specific window's setup fails even though the process-wide
/// setup succeeded — both cases mean the caller should keep using its
/// existing GDI painting for this window, unchanged.
pub fn create_surface(hwnd: HWND, width: i32, height: i32) -> Option<GpuSurface> {
    GPU.with(|g| {
        let g = g.borrow();
        let ctx = g.as_ref()?;
        match try_create_surface(ctx, hwnd, width, height) {
            Ok(surface) => Some(surface),
            Err(e) => {
                tracing::warn!(error = ?e, ?hwnd, width, height, "per-window GPU surface setup failed, falling back to GDI for this window");
                None
            }
        }
    })
}

fn try_create_surface(ctx: &GpuContext, hwnd: HWND, width: i32, height: i32) -> windows::core::Result<GpuSurface> {
    // SAFETY: `hwnd` is a valid, process-lifetime window; the device
    // calls below have no other preconditions.
    unsafe {
        let target = ctx.dcomp_device.CreateTargetForHwnd(hwnd, true)?;
        let (visual, surface) = try_create_visual(ctx, width, height)?;
        target.SetRoot(&visual)?;
        ctx.dcomp_device.Commit()?;
        Ok(GpuSurface { target: Some(target), visual, surface, width, height })
    }
}

/// SAFETY: the caller must be inside an `unsafe` block for the device
/// calls; `ctx` is the process-wide device, alive for the process.
unsafe fn try_create_visual(
    ctx: &GpuContext,
    width: i32,
    height: i32,
) -> windows::core::Result<(IDCompositionVisual2, IDCompositionSurface)> {
    let visual = ctx.dcomp_device.CreateVisual()?;
    let surface = ctx.dcomp_device.CreateSurface(
        width as u32,
        height as u32,
        DXGI_FORMAT_B8G8R8A8_UNORM,
        DXGI_ALPHA_MODE_PREMULTIPLIED,
    )?;
    visual.SetContent(&surface)?;
    Ok((visual, surface))
}

/// Creates an additional surface for a window that already has one.
///
/// A window can hold exactly **one** `IDCompositionTarget`; a second
/// `CreateTargetForHwnd` fails with
/// `DCOMPOSITION_ERROR_WINDOW_ALREADY_COMPOSED`. A surface that is going
/// to be attached under another surface's visual therefore must not ask
/// for a target of its own — it reaches the screen through its parent.
///
/// The caller is responsible for attaching the returned surface's visual
/// with `AddVisual`; until it does, the surface renders nowhere.
pub fn create_child_surface(width: i32, height: i32) -> Option<GpuSurface> {
    GPU.with(|g| {
        let g = g.borrow();
        let ctx = g.as_ref()?;
        // SAFETY: `ctx` is the process-wide device, alive for the process.
        match unsafe { try_create_visual(ctx, width, height) } {
            Ok((visual, surface)) => {
                Some(GpuSurface { target: None, visual, surface, width, height })
            }
            Err(e) => {
                tracing::warn!(error = ?e, width, height, "child composition surface setup failed");
                None
            }
        }
    })
}

/// Sets `surface`'s root visual opacity (0.0–1.0) and commits. No-op if
/// the process-wide GPU setup isn't available.
#[allow(dead_code)] // will be called by overview GPU state in later tasks
pub fn set_opacity(surface: &GpuSurface, opacity: f32) {
    GPU.with(|g| {
        let g = g.borrow();
        let Some(ctx) = g.as_ref() else { return };
        // SAFETY: `surface.visual` was created by this module's own
        // `try_create_surface` and is alive for as long as the caller's
        // `GpuSurface` is. IDCompositionVisual2 can be queried for
        // IDCompositionVisual3 which exposes SetOpacity2.
        unsafe {
            if let Ok(visual3) = surface.visual.cast::<IDCompositionVisual3>() {
                let _ = visual3.SetOpacity2(opacity);
                let _ = ctx.dcomp_device.Commit();
            }
        }
    });
}

/// Commits the process-wide compositor device. No-op if the process-wide
/// GPU setup isn't available. Used by callers that only touched visuals
/// or transforms directly and didn't already go through `redraw`/
/// `set_opacity`, both of which commit internally.
pub fn commit() {
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

use windows::Foundation::Numerics::Matrix3x2;
use windows::Win32::Foundation::POINT;
use windows::Win32::Graphics::Direct2D::Common::{D2D1_COLOR_F, D2D_RECT_F};
use windows::Win32::Graphics::Direct2D::{
    ID2D1DeviceContext, D2D1_DRAW_TEXT_OPTIONS_CLIP, D2D1_DRAW_TEXT_OPTIONS_NONE,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT_NORMAL,
    DWRITE_MEASURING_MODE_NATURAL, DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_CENTER,
    DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_ALIGNMENT_TRAILING, DWRITE_TRIMMING,
    DWRITE_TRIMMING_GRANULARITY_CHARACTER, DWRITE_WORD_WRAPPING_NO_WRAP,
};

/// Draws into `surface` via `draw`, then commits so the compositor picks
/// up the new content. `draw` receives a device context whose origin is
/// already adjusted for `BeginDraw`'s update offset, so callers can draw
/// as if the surface's own top-left were always `(0, 0)`.
pub fn redraw<F>(surface: &GpuSurface, draw: F)
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
            d2d_ctx.SetTransform(&translate);
            draw(&d2d_ctx);
            let _ = surface.surface.EndDraw();
            let _ = ctx.dcomp_device.Commit();
        }
    });
}

pub fn fill_rect(ctx: &ID2D1DeviceContext, rect: D2D_RECT_F, colorref: u32) {
    // SAFETY: `ctx` is a live device context between `BeginDraw`/`EndDraw`
    // (enforced by `redraw`'s closure scope).
    unsafe {
        if let Ok(brush) = ctx.CreateSolidColorBrush(&colorref_to_d2d(colorref), None) {
            ctx.FillRectangle(&rect, &brush);
        }
    }
}

/// Clears the whole target to fully transparent — used by surfaces that
/// only partially cover their window (the floating desktop dock draws a
/// panel at the bottom and leaves the headroom above it see-through) so
/// stale pixels from a previous, differently-sized frame never linger.
pub fn clear_transparent(ctx: &ID2D1DeviceContext) {
    let transparent = D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 };
    // SAFETY: `ctx` is a live device context between `BeginDraw`/`EndDraw`.
    unsafe {
        ctx.Clear(Some(&transparent));
    }
}

/// Fills `rect` with `colorref` at the given `alpha` (0..1) — used for
/// the overview backdrop's dim scrim, which needs partial transparency
/// that `fill_rect`'s always-opaque brush can't express.
pub fn fill_rect_alpha(ctx: &ID2D1DeviceContext, rect: D2D_RECT_F, colorref: u32, alpha: f32) {
    // SAFETY: `ctx` is a live device context between `BeginDraw`/`EndDraw`.
    unsafe {
        let mut color = colorref_to_d2d(colorref);
        color.a = alpha.clamp(0.0, 1.0);
        if let Ok(brush) = ctx.CreateSolidColorBrush(&color, None) {
            ctx.FillRectangle(&rect, &brush);
        }
    }
}

/// Draws `bitmap` stretched to fill `rect` with linear interpolation and
/// no rounded-rect clip — the overview backdrop upscales a small,
/// smoothly-downscaled wallpaper this way, and the linear upscale is
/// exactly what turns the downscaled source into a soft, even blur.
pub fn draw_bitmap_stretched(ctx: &ID2D1DeviceContext, rect: D2D_RECT_F, bitmap: &ID2D1Bitmap) {
    // SAFETY: `ctx` is a live device context between `BeginDraw`/`EndDraw`;
    // `bitmap` is owned by the caller for the duration of the call.
    unsafe {
        ctx.DrawBitmap(bitmap, Some(&rect), 1.0, D2D1_INTERPOLATION_MODE_LINEAR, None, None);
    }
}

pub fn draw_text(
    ctx: &ID2D1DeviceContext,
    rect: D2D_RECT_F,
    text: &str,
    colorref: u32,
    size: f32,
    center_horizontally: bool,
) {
    draw_text_in_font(ctx, rect, text, colorref, size, center_horizontally, "Segoe UI");
}

/// [`draw_text`] with an explicit font family — the bar needs the Windows
/// 11 UI face and the Segoe Fluent Icons face, not just the default.
/// Where a line sits horizontally in its rectangle.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TextAlign {
    Leading,
    Center,
    Trailing,
}

pub fn draw_text_in_font(
    ctx: &ID2D1DeviceContext,
    rect: D2D_RECT_F,
    text: &str,
    colorref: u32,
    size: f32,
    center_horizontally: bool,
    family: &str,
) {
    draw_text_aligned(
        ctx,
        rect,
        text,
        colorref,
        size,
        if center_horizontally { TextAlign::Center } else { TextAlign::Leading },
        false,
        family,
    );
}

/// `draw_text_in_font` with the two things a settings row needs that the
/// shell's own panels never did: a real trailing alignment, and
/// single-line ellipsis trimming.
///
/// Without `ellipsize`, DirectWrite's default is to *wrap*, and with the
/// paragraph alignment centered a description too long for its rect grows
/// a second line that bleeds out of the band it was given — it does not
/// clip, because `D2D1_DRAW_TEXT_OPTIONS_NONE` does not clip either.
pub fn draw_text_aligned(
    ctx: &ID2D1DeviceContext,
    rect: D2D_RECT_F,
    text: &str,
    colorref: u32,
    size: f32,
    align: TextAlign,
    ellipsize: bool,
    family: &str,
) {
    GPU.with(|g| {
        let g = g.borrow();
        let Some(gpu_ctx) = g.as_ref() else { return };
        // SAFETY: same as `fill_rect`; `gpu_ctx.dwrite_factory` is the
        // process-wide factory from `init`, alive for the process's life.
        unsafe {
            let family_wide = windows::core::HSTRING::from(family);
            let Ok(format) = gpu_ctx.dwrite_factory.CreateTextFormat(
                windows::core::PCWSTR(family_wide.as_ptr()),
                None,
                DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                size,
                windows::core::w!("en-us"),
            ) else {
                return;
            };
            let _ = format.SetTextAlignment(match align {
                TextAlign::Center => DWRITE_TEXT_ALIGNMENT_CENTER,
                TextAlign::Trailing => DWRITE_TEXT_ALIGNMENT_TRAILING,
                TextAlign::Leading => DWRITE_TEXT_ALIGNMENT_LEADING,
            });
            let _ = format.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER);

            let mut options = D2D1_DRAW_TEXT_OPTIONS_NONE;
            if ellipsize {
                let _ = format.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP);
                if let Ok(sign) = gpu_ctx.dwrite_factory.CreateEllipsisTrimmingSign(&format) {
                    let trimming = DWRITE_TRIMMING {
                        granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
                        delimiter: 0,
                        delimiterCount: 0,
                    };
                    let _ = format.SetTrimming(&trimming, &sign);
                }
                // Belt and braces: trimming decides where the ellipsis
                // goes, clipping guarantees nothing escapes the rect.
                options = D2D1_DRAW_TEXT_OPTIONS_CLIP;
            }

            let Ok(brush) = ctx.CreateSolidColorBrush(&colorref_to_d2d(colorref), None) else {
                return;
            };
            let wide: Vec<u16> = text.encode_utf16().collect();
            ctx.DrawText(
                &wide,
                &format,
                &rect,
                &brush,
                options,
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

use windows::Win32::Graphics::Direct2D::{
    D2D1_ANTIALIAS_MODE_PER_PRIMITIVE, D2D1_INTERPOLATION_MODE_LINEAR, D2D1_LAYER_OPTIONS1_NONE,
    D2D1_LAYER_PARAMETERS1, D2D1_ROUNDED_RECT, ID2D1Bitmap, ID2D1Geometry,
};
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
pub fn bitmap_from_hbitmap(ctx: &ID2D1DeviceContext, hbitmap: HBITMAP) -> Option<ID2D1Bitmap> {
    // SAFETY: `hbitmap` is a valid, caller-owned GDI bitmap for the
    // duration of this call; every COM object created here is released
    // when it goes out of scope at the end of the function.
    unsafe {
        let wic: IWICImagingFactory =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER).ok()?;
        let wic_bitmap = wic
            .CreateBitmapFromHBITMAP(hbitmap, None, WICBitmapUsePremultipliedAlpha)
            .ok()?;
        // `ID2D1DeviceContext::CreateBitmapFromWicBitmap` returns the
        // newer `ID2D1Bitmap1`, which is a COM subtype of `ID2D1Bitmap`;
        // `cast` is a same-object `QueryInterface`, guaranteed to
        // succeed here.
        let bitmap1: windows::Win32::Graphics::Direct2D::ID2D1Bitmap1 =
            ctx.CreateBitmapFromWicBitmap(&wic_bitmap, None).ok()?;
        bitmap1.cast().ok()
    }
}

/// Draws `bitmap` stretched to fill `rect`, clipped to a rounded rect
/// of `radius`. Mirrors `overview.rs`'s GDI `StretchBlt`-into-a-
/// `CreateRoundRectRgn`-clip pattern.
pub fn draw_rounded_bitmap(ctx: &ID2D1DeviceContext, rect: D2D_RECT_F, radius: f32, bitmap: &ID2D1Bitmap) {
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
        // `CreateRoundedRectangleGeometry` returns the concrete
        // `ID2D1RoundedRectangleGeometry`; `D2D1_LAYER_PARAMETERS1`'s
        // `geometricMask` field is typed as the base `ID2D1Geometry`, so
        // it needs an explicit (same-object, infallible) `cast`.
        let geometry: ID2D1Geometry = match geometry.cast() {
            Ok(geometry) => geometry,
            Err(_) => return,
        };
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
        // `ID2D1DeviceContext::PushLayer`/`PopLayer` (unlike the
        // `ID2D1RenderTarget` overloads with the same name) don't return
        // `Result` — there's no fallible step between them to guard.
        ctx.PushLayer(&params, &layer);
        ctx.DrawBitmap(bitmap, Some(&rect), 1.0, D2D1_INTERPOLATION_MODE_LINEAR, None, None);
        ctx.PopLayer();
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
/// Like [`fill_rounded_rect`] but at `alpha` (0..1), for surfaces that
/// want to be translucent in a rounded shape.
///
/// Needed where the DWM backdrop can't be used: a window whose visible
/// panel is smaller than the window itself (the dock keeps headroom for
/// its magnification wave) would get the system material across the whole
/// window rect, not just the panel. Painting a translucent fill in the
/// right shape is the alternative — it is see-through, though without
/// DWM's blur, which only the system backdrop can provide.
/// An `ID2D1Bitmap` from a tightly packed 32-bit BGRA buffer — the shape
/// the captured tray-icon pixels are already in, so the bar's D2D path can
/// draw them without a detour through an `HBITMAP`.
pub fn bitmap_from_bgra(
    ctx: &ID2D1DeviceContext,
    pixels: &[u8],
    size: i32,
) -> Option<ID2D1Bitmap> {
    let size = size.max(1);
    let stride = (size as u32).checked_mul(4)?;
    if pixels.len() < (stride as usize) * (size as usize) {
        return None;
    }
    use windows::Win32::Graphics::Direct2D::Common::{
        D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_PIXEL_FORMAT, D2D_SIZE_U,
    };
    use windows::Win32::Graphics::Direct2D::D2D1_BITMAP_PROPERTIES1;

    let props = D2D1_BITMAP_PROPERTIES1 {
        pixelFormat: D2D1_PIXEL_FORMAT {
            format: DXGI_FORMAT_B8G8R8A8_UNORM,
            alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED,
        },
        dpiX: 96.0,
        dpiY: 96.0,
        ..Default::default()
    };
    // SAFETY: `pixels` is at least `stride * size` bytes, which is what
    // `CreateBitmap` reads for the given size and stride; the data is
    // copied into the bitmap, so the borrow ends with this call.
    unsafe {
        ctx.CreateBitmap(
            D2D_SIZE_U { width: size as u32, height: size as u32 },
            Some(pixels.as_ptr() as *const std::ffi::c_void),
            stride,
            &props,
        )
        .ok()
        .and_then(|b| b.cast::<ID2D1Bitmap>().ok())
    }
}

pub fn fill_rounded_rect_alpha(
    ctx: &ID2D1DeviceContext,
    rect: D2D_RECT_F,
    radius: f32,
    colorref: u32,
    alpha: f32,
) {
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
        let mut color = colorref_to_d2d(colorref);
        color.a = alpha.clamp(0.0, 1.0);
        if let Ok(brush) = ctx.CreateSolidColorBrush(&color, None) {
            ctx.FillGeometry(&geometry, &brush, None);
        }
    }
}

pub fn fill_rounded_rect(ctx: &ID2D1DeviceContext, rect: D2D_RECT_F, radius: f32, colorref: u32) {
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
pub fn stroke_rounded_rect(
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
        let Some(geometry) = GPU.with(|g| {
            let g = g.borrow();
            let ctx = g.as_ref()?;
            ctx.d2d_factory
                .CreateRoundedRectangleGeometry(&D2D1_ROUNDED_RECT {
                    rect: inset_rect,
                    radiusX: radius,
                    radiusY: radius,
                })
                .ok()
        }) else {
            return;
        };
        let mut color = colorref_to_d2d(colorref);
        color.a = alpha.clamp(0.0, 1.0);
        if let Ok(brush) = ctx.CreateSolidColorBrush(&color, None) {
            ctx.DrawGeometry(&geometry, &brush, stroke_width, None);
        }
    }
}
