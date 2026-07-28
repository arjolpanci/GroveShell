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
pub(crate) enum Icon {
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

impl Icon {
    fn bytes(self) -> &'static [u8] {
        match self {
            Icon::Wifi => include_bytes!("../../resources/icons/png/wifi.png"),
            Icon::WifiOff => include_bytes!("../../resources/icons/png/wifi-off.png"),
            Icon::Volume2 => include_bytes!("../../resources/icons/png/volume-2.png"),
            Icon::Volume1 => include_bytes!("../../resources/icons/png/volume-1.png"),
            Icon::VolumeX => include_bytes!("../../resources/icons/png/volume-x.png"),
            Icon::Bluetooth => include_bytes!("../../resources/icons/png/bluetooth.png"),
            Icon::BluetoothOff => include_bytes!("../../resources/icons/png/bluetooth-off.png"),
            Icon::Plane => include_bytes!("../../resources/icons/png/plane.png"),
            Icon::BatteryFull => include_bytes!("../../resources/icons/png/battery-full.png"),
            Icon::BatteryMedium => include_bytes!("../../resources/icons/png/battery-medium.png"),
            Icon::BatteryLow => include_bytes!("../../resources/icons/png/battery-low.png"),
            Icon::BatteryWarning => include_bytes!("../../resources/icons/png/battery-warning.png"),
            Icon::BatteryCharging => include_bytes!("../../resources/icons/png/battery-charging.png"),
            Icon::Sun => include_bytes!("../../resources/icons/png/sun.png"),
            Icon::Moon => include_bytes!("../../resources/icons/png/moon.png"),
        }
    }
}

/// Picks the volume glyph that matches an actual level rather than
/// just "on/off" — muted always wins, then a rough two-way split
/// between "quiet" and "loud" since Lucide only ships one mid-level
/// variant (`volume-1`) between silent and full.
pub(crate) fn volume_icon(muted: bool, percent: u32) -> Icon {
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
pub(crate) fn battery_icon(percent: u8, charging: bool) -> Icon {
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

thread_local! {
    /// Decoded once per icon, kept for the process lifetime — same
    /// tradeoff as `WALLPAPER_BITMAP`/the dock's pinned-icon cache
    /// elsewhere in this codebase (a handful of small bitmaps, never
    /// worth tearing down).
    static CACHE: RefCell<HashMap<Icon, isize>> = RefCell::new(HashMap::new());
}

/// Decodes `icon`'s embedded PNG bytes into a `GpBitmap` via an
/// in-memory `IStream` (there's no `GdipCreateBitmapFromFile` for bytes
/// that were never a real file). Cached after the first call.
fn bitmap_for(icon: Icon) -> Option<*mut GpBitmap> {
    if let Some(existing) = CACHE.with(|c| c.borrow().get(&icon).copied()) {
        return Some(existing as *mut GpBitmap);
    }
    let bytes = icon.bytes();
    // SAFETY: `SHCreateMemStream` copies `bytes` into its own
    // heap-owned buffer, so the stream is valid independent of this
    // function's stack frame; `GdipCreateBitmapFromStream` reads the
    // whole stream synchronously before returning.
    unsafe {
        let stream: IStream = SHCreateMemStream(Some(bytes))?;
        let mut bitmap: *mut GpBitmap = std::ptr::null_mut();
        let status = GdipCreateBitmapFromStream(&stream, &mut bitmap);
        if status.0 != 0 || bitmap.is_null() {
            return None;
        }
        CACHE.with(|c| c.borrow_mut().insert(icon, bitmap as isize));
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
pub(crate) unsafe fn draw_icon(hdc: HDC, rect: RECT, icon: Icon, color: COLORREF) {
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
