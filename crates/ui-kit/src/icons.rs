//! Real icon assets instead of hand-drawn GDI glyphs — see
//! `resources/icons/NOTICE.md` for where they come from and their
//! license. Each PNG is embedded into the binary at compile time
//! (`include_bytes!`, so there's no install-time "where's the icons
//! folder" question) and decoded through GDI+ once, then cached for
//! the process lifetime the same way `overview.rs` caches the
//! wallpaper bitmap. Every icon ships white-on-transparent; recoloring
//! to whatever the caller actually wants (the normal text color, the
//! muted "unavailable" gray, the low-battery red, ...) happens at draw
//! time via a GDI+ color matrix rather than needing a pre-rendered copy
//! per color.

use std::cell::RefCell;
use std::collections::HashMap;

use windows::Win32::Foundation::{COLORREF, RECT};
use windows::Win32::Graphics::Gdi::HDC;
use windows::Win32::Graphics::GdiPlus::{
    GdipCreateBitmapFromStream, GdipCreateFromHDC, GdipCreateImageAttributes,
    GdipDeleteGraphics, GdipDisposeImageAttributes, GdipDrawImageRectRectI,
    GdipSetImageAttributesColorMatrix, ColorAdjustTypeDefault, ColorMatrix, ColorMatrixFlagsDefault,
    GpBitmap, GpImage, Ok as GdipOk, UnitPixel,
};
use windows::Win32::System::Com::IStream;
use windows::Win32::UI::Shell::SHCreateMemStream;

/// One entry per file under `resources/icons/png/`. Deliberately named
/// after states, not generic shapes — callers pick the variant that
/// matches the state they already know, rather than this module
/// guessing thresholds itself.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub enum Icon {
    Wifi,
    WifiOff,
    Volume2,
    Volume1,
    VolumeX,
    Bluetooth,
    BluetoothOff,
    Plane,
    BatteryFull,
    BatteryMedium,
    BatteryLow,
    BatteryWarning,
    BatteryCharging,
    Sun,
    Moon,
}

/// The Segoe Fluent Icons codepoint for an icon, or `None` when the font
/// has no suitable glyph and the bundled PNG should be used instead.
///
/// Every codepoint here was verified against the installed font rather
/// than taken from a table: each candidate was selected into a DC and
/// checked with `GetGlyphIndicesW` for existence, then *rendered and
/// looked at* to confirm it means what its name suggests. That second
/// step matters — `E75E` exists and sounds plausible for "bluetooth off"
/// but draws a device pill, and the battery run turned out to be `E850`
/// (empty) through `E859` (full) with the charging variants living
/// separately at `E83E`/`E83F`.
pub fn fluent_glyph(icon: Icon) -> Option<&'static str> {
    Some(match icon {
        Icon::Wifi => "\u{E701}",
        Icon::WifiOff => "\u{EB5E}",
        Icon::Volume2 => "\u{E767}",
        Icon::Volume1 => "\u{E993}",
        Icon::VolumeX => "\u{E74F}",
        Icon::Bluetooth => "\u{E702}",
        // Segoe Fluent has no "bluetooth disabled" glyph; keep the PNG.
        Icon::BluetoothOff => return None,
        Icon::Plane => "\u{E709}",
        Icon::BatteryFull => "\u{E83F}",
        Icon::BatteryMedium => "\u{E855}",
        Icon::BatteryLow => "\u{E852}",
        Icon::BatteryWarning => "\u{E996}",
        Icon::BatteryCharging => "\u{E83E}",
        Icon::Sun => "\u{E706}",
        Icon::Moon => "\u{E708}",
    })
}

impl Icon {
    fn bytes(self) -> &'static [u8] {
        match self {
            Icon::Wifi => include_bytes!("../resources/icons/png/wifi.png"),
            Icon::WifiOff => include_bytes!("../resources/icons/png/wifi-off.png"),
            Icon::Volume2 => include_bytes!("../resources/icons/png/volume-2.png"),
            Icon::Volume1 => include_bytes!("../resources/icons/png/volume-1.png"),
            Icon::VolumeX => include_bytes!("../resources/icons/png/volume-x.png"),
            Icon::Bluetooth => include_bytes!("../resources/icons/png/bluetooth.png"),
            Icon::BluetoothOff => include_bytes!("../resources/icons/png/bluetooth-off.png"),
            Icon::Plane => include_bytes!("../resources/icons/png/plane.png"),
            Icon::BatteryFull => include_bytes!("../resources/icons/png/battery-full.png"),
            Icon::BatteryMedium => include_bytes!("../resources/icons/png/battery-medium.png"),
            Icon::BatteryLow => include_bytes!("../resources/icons/png/battery-low.png"),
            Icon::BatteryWarning => include_bytes!("../resources/icons/png/battery-warning.png"),
            Icon::BatteryCharging => include_bytes!("../resources/icons/png/battery-charging.png"),
            Icon::Sun => include_bytes!("../resources/icons/png/sun.png"),
            Icon::Moon => include_bytes!("../resources/icons/png/moon.png"),
        }
    }
}

/// Picks the volume glyph that matches an actual level rather than
/// just "on/off" — muted always wins, then a rough two-way split
/// between "quiet" and "loud" since Lucide only ships one mid-level
/// variant (`volume-1`) between silent and full.
pub fn volume_icon(muted: bool, percent: u32) -> Icon {
    if muted || percent == 0 {
        Icon::VolumeX
    } else if percent < 50 {
        Icon::Volume1
    } else {
        Icon::Volume2
    }
}

/// Picks the battery glyph for a percentage/charging pair — charging
/// always shows the bolt icon regardless of level, matching how
/// Windows' own battery icon behaves.
pub fn battery_icon(percent: u8, charging: bool) -> Icon {
    if charging {
        Icon::BatteryCharging
    } else if percent <= 15 {
        Icon::BatteryWarning
    } else if percent <= 40 {
        Icon::BatteryLow
    } else if percent <= 75 {
        Icon::BatteryMedium
    } else {
        Icon::BatteryFull
    }
}

struct CachedBitmap {
    bitmap: isize,
    _stream: IStream,
}

impl Drop for CachedBitmap {
    fn drop(&mut self) {
        // SAFETY: the bitmap belongs to this cache entry; its backing
        // stream remains alive until after this Drop implementation.
        unsafe { let _ = windows::Win32::Graphics::GdiPlus::GdipDisposeImage(self.bitmap as *mut GpImage); }
    }
}

thread_local! {
    /// Decoded once per icon, kept for the process lifetime — same
    /// tradeoff as `WALLPAPER_BITMAP`/the dock's pinned-icon cache
    /// elsewhere in this codebase (a handful of small bitmaps, never
    /// worth tearing down).
    static CACHE: RefCell<HashMap<Icon, CachedBitmap>> = RefCell::new(HashMap::new());
}

/// Decodes `icon`'s embedded PNG bytes into a `GpBitmap` via an
/// in-memory `IStream` (there's no `GdipCreateBitmapFromFile` for bytes
/// that were never a real file). Cached after the first call.
fn bitmap_for(icon: Icon) -> Option<*mut GpBitmap> {
    if let Some(existing) = CACHE.with(|c| c.borrow().get(&icon).map(|entry| entry.bitmap)) {
        return Some(existing as *mut GpBitmap);
    }
    let bytes = icon.bytes();
    // SAFETY: `SHCreateMemStream` copies `bytes` into its own
    // heap-owned buffer, so the stream is valid independent of this
    // function's stack frame. GDI+ can decode lazily, so the stream must
    // remain alive for the lifetime of the cached bitmap.
    unsafe {
        let stream: IStream = SHCreateMemStream(Some(bytes))?;
        let mut bitmap: *mut GpBitmap = std::ptr::null_mut();
        let status = GdipCreateBitmapFromStream(&stream, &mut bitmap);
        if status.0 != 0 || bitmap.is_null() {
            return None;
        }
        CACHE.with(|c| c.borrow_mut().insert(icon, CachedBitmap { bitmap: bitmap as isize, _stream: stream }));
        Some(bitmap)
    }
}

/// Draws `icon` into `rect`, recolored to `color` (the source PNGs are
/// all white-on-transparent, so this is just scaling the R/G/B channels
/// down from 1.0 to whatever fraction `color` calls for — a plain
/// diagonal color matrix, alpha untouched).
///
/// SAFETY: `hdc` must be a valid device context currently being painted
/// into.
pub unsafe fn draw_icon(hdc: HDC, rect: RECT, icon: Icon, color: COLORREF) {
    // Prefer the system icon font: it is what every other Windows 11
    // surface draws, and it scales as an outline instead of stretching a
    // fixed-size bitmap. The bundled PNGs stay as the fallback for
    // pre-Windows-11 machines and for the one state Fluent has no glyph
    // for (see `fluent_glyph`).
    if crate::design::typography::icon_font_available() {
        if let Some(glyph) = fluent_glyph(icon) {
            draw_glyph(hdc, rect, glyph, color);
            return;
        }
    }
    let Some(bitmap) = bitmap_for(icon) else {
        return;
    };
    let r = (color.0 & 0xFF) as f32 / 255.0;
    let g = ((color.0 >> 8) & 0xFF) as f32 / 255.0;
    let b = ((color.0 >> 16) & 0xFF) as f32 / 255.0;
    #[rustfmt::skip]
    let matrix = ColorMatrix {
        m: [
            r,   0.0, 0.0, 0.0, 0.0,
            0.0, g,   0.0, 0.0, 0.0,
            0.0, 0.0, b,   0.0, 0.0,
            0.0, 0.0, 0.0, 1.0, 0.0,
            0.0, 0.0, 0.0, 0.0, 1.0,
        ],
    };

    let mut attributes = std::ptr::null_mut();
    if GdipCreateImageAttributes(&mut attributes) != GdipOk || attributes.is_null() {
        return;
    }
    let _ = GdipSetImageAttributesColorMatrix(
        attributes,
        ColorAdjustTypeDefault,
        true,
        &matrix,
        std::ptr::null(),
        ColorMatrixFlagsDefault,
    );

    let mut graphics = std::ptr::null_mut();
    if GdipCreateFromHDC(hdc, &mut graphics) == GdipOk && !graphics.is_null() {
        let width = rect.right - rect.left;
        let height = rect.bottom - rect.top;
        let _ = GdipDrawImageRectRectI(
            graphics,
            bitmap as *mut GpImage,
            rect.left,
            rect.top,
            width,
            height,
            0,
            0,
            128,
            128,
            UnitPixel,
            attributes,
            0,
            std::ptr::null_mut(),
        );
        let _ = GdipDeleteGraphics(graphics);
    }
    let _ = GdipDisposeImageAttributes(attributes);
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Icon; 15] = [
        Icon::Wifi, Icon::WifiOff, Icon::Volume2, Icon::Volume1, Icon::VolumeX,
        Icon::Bluetooth, Icon::BluetoothOff, Icon::Plane, Icon::BatteryFull,
        Icon::BatteryMedium, Icon::BatteryLow, Icon::BatteryWarning,
        Icon::BatteryCharging, Icon::Sun, Icon::Moon,
    ];

    #[test]
    fn every_icon_but_bluetooth_off_has_a_fluent_glyph() {
        // Segoe Fluent Icons has no "bluetooth disabled" glyph — verified
        // by rendering the candidates (E703/E704/E75B turned out to be a
        // device pair, a broadcast tower, and a card). That one variant
        // keeps its PNG.
        for icon in ALL {
            let glyph = fluent_glyph(icon);
            if icon == Icon::BluetoothOff {
                assert!(glyph.is_none(), "BluetoothOff should fall back to its PNG");
            } else {
                assert!(glyph.is_some(), "missing Fluent glyph for an icon");
            }
        }
    }

    #[test]
    fn fluent_glyphs_are_single_chars_in_the_private_use_area() {
        // A multi-char or out-of-range entry would render as tofu or as
        // literal text in the bar, so pin both properties.
        for icon in ALL {
            let Some(glyph) = fluent_glyph(icon) else { continue };
            let mut chars = glyph.chars();
            let c = chars.next().expect("glyph must not be empty");
            assert!(chars.next().is_none(), "glyph must be exactly one char");
            assert!(
                ('\u{E700}'..='\u{F8FF}').contains(&c),
                "glyph outside the Segoe Fluent private use area"
            );
        }
    }

    #[test]
    fn battery_glyphs_rise_with_charge_level() {
        // The battery run is contiguous (E850 empty -> E859 full), so a
        // transposed mapping would silently show a full battery at 5%.
        let level = |icon| fluent_glyph(icon).unwrap().chars().next().unwrap() as u32;
        assert!(level(Icon::BatteryLow) < level(Icon::BatteryMedium));
    }
}

/// Draws one Segoe Fluent Icons glyph centered in `rect`, sized to the
/// rect's height so it matches whatever the PNG path would have drawn.
///
/// SAFETY: `hdc` must be a valid device context the caller is painting
/// into; the font and text state are restored before returning.
unsafe fn draw_glyph(hdc: HDC, rect: RECT, glyph: &str, color: COLORREF) {
    use windows::core::{HSTRING, PCWSTR};
    use windows::Win32::Graphics::Gdi::{
        CreateFontW, DeleteObject, SelectObject, SetBkMode, SetTextColor, CLEARTYPE_QUALITY,
        CLIP_DEFAULT_PRECIS, DEFAULT_CHARSET, DEFAULT_PITCH, DrawTextW, OUT_DEFAULT_PRECIS,
        DT_CENTER, DT_NOPREFIX, DT_SINGLELINE, DT_VCENTER, TRANSPARENT,
    };

    let face = HSTRING::from(crate::design::typography::ICON_FACE);
    let size = (rect.bottom - rect.top).max(1);
    let font = CreateFontW(
        -size,
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
        PCWSTR(face.as_ptr()),
    );
    let previous_font = SelectObject(hdc, font);
    let previous_mode = SetBkMode(hdc, TRANSPARENT);
    let previous_color = SetTextColor(hdc, color);

    let mut wide: Vec<u16> = glyph.encode_utf16().collect();
    let mut r = rect;
    DrawTextW(hdc, &mut wide, &mut r, DT_CENTER | DT_VCENTER | DT_SINGLELINE | DT_NOPREFIX);

    SetTextColor(hdc, previous_color);
    SetBkMode(hdc, windows::Win32::Graphics::Gdi::BACKGROUND_MODE(previous_mode as u32));
    SelectObject(hdc, previous_font);
    let _ = DeleteObject(font);
}

/// Draws a bare Segoe Fluent Icons codepoint into `rect`, for the bar's
/// chrome glyphs (settings, session) that have no `Icon` variant or PNG
/// behind them. Returns `false` when the icon font is unavailable, so the
/// caller can draw its pre-Windows-11 text fallback instead.
///
/// SAFETY: `hdc` must be a valid device context the caller is painting
/// into; see [`draw_glyph`].
pub unsafe fn draw_fluent_glyph(hdc: HDC, rect: RECT, glyph: &str, color: COLORREF) -> bool {
    if !crate::design::typography::icon_font_available() {
        return false;
    }
    draw_glyph(hdc, rect, glyph, color);
    true
}
