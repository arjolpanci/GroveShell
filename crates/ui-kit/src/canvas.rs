//! One drawing interface, two backends.
//!
//! The shell has two ways to put pixels on screen: GDI, which every
//! surface started on, and Direct2D on a DirectComposition surface, which
//! is the only one that can be translucent — GDI has no alpha channel, so
//! every pixel it writes is opaque and a DWM backdrop can never show
//! through it.
//!
//! The top bar solved that with a second painter, which is fine for a
//! surface whose drawing is a few dozen lines. Quick Settings has ~570
//! lines across five pages, and two copies of that would drift apart the
//! first time anyone touched it. So the panel is written **once** against
//! this trait and the backend decides whether the result is opaque GDI or
//! translucent Direct2D.
//!
//! The interface is deliberately stateful — a current text color and font
//! size, set and then used by later `text` calls — because that is how the
//! GDI code it replaces was written. Mirroring that shape kept the
//! conversion mechanical and compiler-checked instead of a hand rewrite of
//! every call site.
//!
//! Coordinates are window-client pixels, already DPI-scaled by the caller:
//! the same values the layout functions and hit-testing use, so what is
//! drawn and what is clickable cannot drift apart.

use windows::Win32::Foundation::{COLORREF, RECT};
use windows::Win32::Graphics::Gdi::DRAW_TEXT_FORMAT;

use crate::icons::Icon;

/// The drawing operations the shell's panels actually use.
///
/// Deliberately small: this is the set the panels need, not a general 2D
/// API. Anything added here has to be implementable on both backends.
pub trait Canvas {
    /// Sets the color later `text`, `icon` and `glyph` calls draw in.
    fn set_text_color(&mut self, color: COLORREF);

    /// Sets the logical-pixel size later `text` calls draw at. The
    /// backend applies DPI scaling itself.
    fn set_font_size(&mut self, size_px: i32);

    /// Fills a plain rectangle.
    fn fill_rect(&mut self, rect: RECT, color: COLORREF);

    /// Fills a rounded rectangle. A `radius` of 0 is a plain rectangle.
    fn fill_round_rect(&mut self, rect: RECT, radius: i32, color: COLORREF);

    /// Fills a rounded rectangle at `alpha` (0..1).
    ///
    /// On the GDI backend alpha is ignored and the fill is opaque; that
    /// backend has no way to express it, which is the whole reason the
    /// Direct2D one exists.
    fn fill_round_rect_alpha(&mut self, rect: RECT, radius: i32, color: COLORREF, alpha: f32);

    /// Strokes a rounded rectangle just inside `rect`.
    fn stroke_round_rect(&mut self, rect: RECT, radius: i32, color: COLORREF, width: f32);

    /// Fills an ellipse bounded by `rect`.
    fn fill_ellipse(&mut self, rect: RECT, color: COLORREF);

    /// Draws a single line of text in the current color and size.
    /// Honors `DT_CENTER` / `DT_RIGHT`; everything else is left-aligned
    /// and vertically centered, which is every label the panels draw.
    fn text(&mut self, rect: RECT, s: &str, flags: DRAW_TEXT_FORMAT);

    /// Draws one shell icon in the current color, sized to `rect`.
    fn icon_colored(&mut self, rect: RECT, icon: Icon, color: COLORREF);

    /// Draws a bare Segoe Fluent Icons glyph in the current color.
    fn glyph(&mut self, rect: RECT, glyph: &str);

    /// Restricts later drawing to `rect` until [`Canvas::pop_clip`].
    ///
    /// A scrolling surface needs this: its content is drawn at an offset
    /// and would otherwise paint over whatever chrome sits above the
    /// scrolling area. Calls pair strictly.
    fn push_clip(&mut self, rect: RECT);

    /// Undoes the most recent [`Canvas::push_clip`].
    fn pop_clip(&mut self);
}

// ---------------------------------------------------------------------
// GDI backend
// ---------------------------------------------------------------------

/// Draws onto a GDI device context. Opaque by construction.
pub struct GdiCanvas {
    hdc: windows::Win32::Graphics::Gdi::HDC,
    dpi: u32,
    color: u32,
    font_px: i32,
    /// `SaveDC` handles, one per unmatched `push_clip`.
    saved_states: Vec<i32>,
}

impl GdiCanvas {
    /// SAFETY: `hdc` must be a valid device context for as long as this
    /// canvas is used.
    pub unsafe fn new(hdc: windows::Win32::Graphics::Gdi::HDC, dpi: u32) -> Self {
        use windows::Win32::Graphics::Gdi::{SetBkMode, TRANSPARENT};
        SetBkMode(hdc, TRANSPARENT);
        Self {
            hdc,
            dpi,
            color: 0,
            font_px: crate::design::typography::BODY_PX,
            saved_states: Vec::new(),
        }
    }
}

impl Canvas for GdiCanvas {
    fn set_text_color(&mut self, color: COLORREF) {
        self.color = color.0;
    }

    fn set_font_size(&mut self, size_px: i32) {
        self.font_px = size_px.max(1);
    }

    fn fill_rect(&mut self, rect: RECT, color: COLORREF) {
        use windows::Win32::Graphics::Gdi::{CreateSolidBrush, DeleteObject, FillRect};
        // SAFETY: `self.hdc` is valid by this type's construction
        // contract; the brush is deleted before returning.
        unsafe {
            let brush = CreateSolidBrush(color);
            FillRect(self.hdc, &rect, brush);
            let _ = DeleteObject(brush);
        }
    }

    fn fill_round_rect(&mut self, rect: RECT, radius: i32, color: COLORREF) {
        use windows::Win32::Graphics::Gdi::{
            CreateSolidBrush, DeleteObject, GetStockObject, RoundRect, SelectObject, NULL_PEN,
        };
        // SAFETY: as `fill_rect`; every object is deselected and deleted.
        unsafe {
            let brush = CreateSolidBrush(color);
            let previous_brush = SelectObject(self.hdc, brush);
            let previous_pen = SelectObject(self.hdc, GetStockObject(NULL_PEN));
            let _ = RoundRect(
                self.hdc,
                rect.left,
                rect.top,
                rect.right,
                rect.bottom,
                radius * 2,
                radius * 2,
            );
            SelectObject(self.hdc, previous_pen);
            SelectObject(self.hdc, previous_brush);
            let _ = DeleteObject(brush);
        }
    }

    fn fill_round_rect_alpha(&mut self, rect: RECT, radius: i32, color: COLORREF, _alpha: f32) {
        // GDI cannot express alpha; an opaque fill is the honest result.
        self.fill_round_rect(rect, radius, color);
    }

    fn stroke_round_rect(&mut self, rect: RECT, radius: i32, color: COLORREF, width: f32) {
        use windows::Win32::Graphics::Gdi::{
            CreatePen, DeleteObject, GetStockObject, RoundRect, SelectObject, HOLLOW_BRUSH,
            PS_SOLID,
        };
        // SAFETY: as `fill_rect`.
        unsafe {
            let pen = CreatePen(PS_SOLID, width.max(1.0) as i32, color);
            let previous_pen = SelectObject(self.hdc, pen);
            let previous_brush = SelectObject(self.hdc, GetStockObject(HOLLOW_BRUSH));
            let _ = RoundRect(
                self.hdc,
                rect.left,
                rect.top,
                rect.right,
                rect.bottom,
                radius * 2,
                radius * 2,
            );
            SelectObject(self.hdc, previous_brush);
            SelectObject(self.hdc, previous_pen);
            let _ = DeleteObject(pen);
        }
    }

    fn fill_ellipse(&mut self, rect: RECT, color: COLORREF) {
        use windows::Win32::Graphics::Gdi::{
            CreateSolidBrush, DeleteObject, Ellipse, GetStockObject, SelectObject, NULL_PEN,
        };
        // SAFETY: as `fill_rect`.
        unsafe {
            let brush = CreateSolidBrush(color);
            let previous_brush = SelectObject(self.hdc, brush);
            let previous_pen = SelectObject(self.hdc, GetStockObject(NULL_PEN));
            let _ = Ellipse(self.hdc, rect.left, rect.top, rect.right, rect.bottom);
            SelectObject(self.hdc, previous_pen);
            SelectObject(self.hdc, previous_brush);
            let _ = DeleteObject(brush);
        }
    }

    fn text(&mut self, rect: RECT, s: &str, flags: DRAW_TEXT_FORMAT) {
        use windows::Win32::Graphics::Gdi::{DeleteObject, SelectObject, SetTextColor};
        // SAFETY: as `fill_rect`; the font is deselected and deleted.
        unsafe {
            SetTextColor(self.hdc, COLORREF(self.color));
            let font = crate::text::ui_font(self.font_px, self.dpi);
            let previous = SelectObject(self.hdc, font);
            crate::text::draw_text_in(self.hdc, rect, s, flags);
            SelectObject(self.hdc, previous);
            let _ = DeleteObject(font);
        }
    }

    fn icon_colored(&mut self, rect: RECT, icon: Icon, color: COLORREF) {
        // SAFETY: as `fill_rect`.
        unsafe {
            crate::icons::draw_icon(self.hdc, rect, icon, color);
        }
    }

    fn glyph(&mut self, rect: RECT, glyph: &str) {
        // SAFETY: as `fill_rect`.
        unsafe {
            crate::icons::draw_fluent_glyph(self.hdc, rect, glyph, COLORREF(self.color));
        }
    }

    fn push_clip(&mut self, rect: RECT) {
        use windows::Win32::Graphics::Gdi::{IntersectClipRect, SaveDC};
        // SAFETY: `self.hdc` is valid by this type's construction
        // contract; `SaveDC`/`RestoreDC` bracket the clip so `pop_clip`
        // restores exactly the state that was current here.
        unsafe {
            self.saved_states.push(SaveDC(self.hdc));
            IntersectClipRect(self.hdc, rect.left, rect.top, rect.right, rect.bottom);
        }
    }

    fn pop_clip(&mut self) {
        use windows::Win32::Graphics::Gdi::RestoreDC;
        let Some(state) = self.saved_states.pop() else { return };
        // SAFETY: `state` came from this DC's own `SaveDC` above.
        unsafe {
            let _ = RestoreDC(self.hdc, state);
        }
    }
}

// ---------------------------------------------------------------------
// Direct2D backend
// ---------------------------------------------------------------------

/// Draws onto a Direct2D device context backed by a DirectComposition
/// surface, so anything not drawn stays transparent and whatever DWM
/// material sits behind the window shows through.
pub struct D2DCanvas<'a> {
    ctx: &'a windows::Win32::Graphics::Direct2D::ID2D1DeviceContext,
    dpi: u32,
    color: u32,
    font_px: i32,
}

impl<'a> D2DCanvas<'a> {
    pub fn new(
        ctx: &'a windows::Win32::Graphics::Direct2D::ID2D1DeviceContext,
        dpi: u32,
    ) -> Self {
        Self { ctx, dpi, color: 0, font_px: crate::design::typography::BODY_PX }
    }

    fn rect(r: RECT) -> windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F {
        windows::Win32::Graphics::Direct2D::Common::D2D_RECT_F {
            left: r.left as f32,
            top: r.top as f32,
            right: r.right as f32,
            bottom: r.bottom as f32,
        }
    }
}

impl Canvas for D2DCanvas<'_> {
    fn push_clip(&mut self, rect: RECT) {
        // SAFETY: `self.ctx` is inside an active `BeginDraw`/`EndDraw`
        // bracket (see `gpu::redraw`), which is what Direct2D requires
        // for a clip; `pop_clip` pairs with this.
        unsafe {
            self.ctx.PushAxisAlignedClip(
                &Self::rect(rect),
                windows::Win32::Graphics::Direct2D::D2D1_ANTIALIAS_MODE_ALIASED,
            );
        }
    }

    fn pop_clip(&mut self) {
        // SAFETY: pairs with `push_clip` above, inside the same draw
        // bracket.
        unsafe {
            self.ctx.PopAxisAlignedClip();
        }
    }

    fn set_text_color(&mut self, color: COLORREF) {
        self.color = color.0;
    }

    fn set_font_size(&mut self, size_px: i32) {
        self.font_px = size_px.max(1);
    }

    fn fill_rect(&mut self, rect: RECT, color: COLORREF) {
        crate::gpu::fill_rect(self.ctx, Self::rect(rect), color.0);
    }

    fn fill_round_rect(&mut self, rect: RECT, radius: i32, color: COLORREF) {
        crate::gpu::fill_rounded_rect(self.ctx, Self::rect(rect), radius as f32, color.0);
    }

    fn fill_round_rect_alpha(&mut self, rect: RECT, radius: i32, color: COLORREF, alpha: f32) {
        crate::gpu::fill_rounded_rect_alpha(self.ctx, Self::rect(rect), radius as f32, color.0, alpha);
    }

    fn stroke_round_rect(&mut self, rect: RECT, radius: i32, color: COLORREF, width: f32) {
        crate::gpu::stroke_rounded_rect(
            self.ctx,
            Self::rect(rect),
            radius as f32,
            color.0,
            1.0,
            width,
        );
    }

    fn fill_ellipse(&mut self, rect: RECT, color: COLORREF) {
        // A circle is a rounded rect whose radius is half its shorter
        // side, so Direct2D needs no separate ellipse primitive.
        let radius = ((rect.right - rect.left).min(rect.bottom - rect.top) / 2).max(0);
        crate::gpu::fill_rounded_rect(self.ctx, Self::rect(rect), radius as f32, color.0);
    }

    fn text(&mut self, rect: RECT, s: &str, flags: DRAW_TEXT_FORMAT) {
        use windows::Win32::Graphics::Gdi::{DT_CENTER, DT_END_ELLIPSIS, DT_RIGHT};
        let align = if (flags.0 & DT_CENTER.0) != 0 {
            crate::gpu::TextAlign::Center
        } else if (flags.0 & DT_RIGHT.0) != 0 {
            crate::gpu::TextAlign::Trailing
        } else {
            crate::gpu::TextAlign::Leading
        };
        crate::gpu::draw_text_aligned(
            self.ctx,
            Self::rect(rect),
            s,
            self.color,
            crate::runtime::scaled(self.font_px, self.dpi) as f32,
            align,
            (flags.0 & DT_END_ELLIPSIS.0) != 0,
            crate::design::typography::resolved_ui_face(),
        );
    }

    fn icon_colored(&mut self, rect: RECT, icon: Icon, color: COLORREF) {
        // The Direct2D path draws icons as glyphs; the bundled PNGs are a
        // GDI-only fallback, and this backend only runs where the Fluent
        // face is present.
        let previous = self.color;
        self.color = color.0;
        if let Some(g) = crate::icons::fluent_glyph(icon) {
            self.glyph(rect, g);
        }
        self.color = previous;
    }

    fn glyph(&mut self, rect: RECT, glyph: &str) {
        let size = (rect.bottom - rect.top).max(1) as f32;
        crate::gpu::draw_text_in_font(
            self.ctx,
            Self::rect(rect),
            glyph,
            self.color,
            size,
            true,
            crate::design::typography::ICON_FACE,
        );
    }
}
