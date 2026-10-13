//! Live window previews in the overview, via DWM thumbnails.
//!
//! The overview used to show `PrintWindow` captures: a still image taken
//! when Activities opened, which then sat frozen while the real window
//! carried on playing video, scrolling, or redrawing. `DwmRegisterThumbnail`
//! asks DWM to composite the *live* contents of a source window into a
//! rectangle of a destination window instead, which is what the Windows
//! and GNOME overviews both do.
//!
//! Two things shape how this is used:
//!
//! - **DWM draws thumbnails above the destination window's own content.**
//!   There is no z-ordering with what the overview paints, and no way to
//!   clip one to a rounded rect. So the painted snapshot stays underneath
//!   as the base layer, and a live thumbnail is laid over it where one is
//!   possible. Nothing flashes empty if a thumbnail cannot be registered.
//!
//! - **A hidden source renders nothing.** Windows parked on another
//!   workspace are hidden with `ShowWindow`, so they have no live contents
//!   to show and keep their park-time capture. Only windows that are
//!   actually on screen get a live preview, which is exactly the set the
//!   user is looking at when Activities opens.
//!
//! Registrations are keyed by destination window, because each monitor has
//! its own overview and the same source window must never be registered
//! into two destinations at once.

use std::cell::RefCell;
use std::collections::HashMap;

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Dwm::{
    DwmRegisterThumbnail, DwmUnregisterThumbnail, DwmUpdateThumbnailProperties,
    DWM_THUMBNAIL_PROPERTIES, DWM_TNP_OPACITY, DWM_TNP_RECTDESTINATION,
    DWM_TNP_SOURCECLIENTAREAONLY, DWM_TNP_VISIBLE,
};
use windows::Win32::UI::WindowsAndMessaging::{IsIconic, IsWindowVisible};

thread_local! {
    /// destination window -> (source window -> live registration).
    static REGISTRY: RefCell<HashMap<isize, HashMap<isize, isize>>> =
        RefCell::new(HashMap::new());
}

/// Whether `source` can show a live preview right now.
///
/// A hidden window (parked on another workspace, or minimized) has no
/// live contents for DWM to composite, so the caller keeps its captured
/// snapshot instead of showing an empty slot.
pub(crate) fn can_preview(source: HWND) -> bool {
    // SAFETY: plain queries; a stale handle reports false rather than
    // misbehaving.
    unsafe { previewable(IsWindowVisible(source).as_bool(), IsIconic(source).as_bool()) }
}

/// Whether DWM has anything live to composite for a source window.
///
/// Minimized is the case that bit: a minimized window still reports
/// `IsWindowVisible == true`, so checking visibility alone registered a
/// thumbnail for it. DWM then composited a blank grey rectangle — above
/// this window's own content, so it covered the snapshot and the title
/// chip that would otherwise have shown, and nothing could paint over
/// it.
pub(crate) fn previewable(visible: bool, iconic: bool) -> bool {
    visible && !iconic
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_on_screen_window_can_be_previewed_live() {
        assert!(previewable(true, false));
    }

    #[test]
    fn a_minimized_window_cannot_even_though_it_reports_visible() {
        // Measured: a minimized Firefox reports visible=true, iconic=true
        // and rect 314x50 at -32000,-32000.
        assert!(!previewable(true, true));
    }

    #[test]
    fn a_hidden_window_cannot() {
        // Parked on another workspace: `ShowWindow(SW_HIDE)`.
        assert!(!previewable(false, false));
        assert!(!previewable(false, true));
    }
}

/// Points `dest`'s live previews at exactly `items`, registering what is
/// new, moving what already existed, and dropping what is gone.
///
/// Called every frame the overview paints, so it is written to do nothing
/// but a cheap property update in the common case — registration only
/// happens the first time a given source appears.
pub(crate) fn sync(dest: HWND, items: &[(HWND, RECT)]) {
    REGISTRY.with(|registry| {
        let mut registry = registry.borrow_mut();
        let entries = registry.entry(dest.0 as isize).or_default();

        for (source, rect) in items {
            let key = source.0 as isize;
            let handle = match entries.get(&key) {
                Some(&existing) => existing,
                None => {
                    // SAFETY: both are live top-level windows; the
                    // returned handle is kept in the registry until
                    // `clear` or the stale sweep unregisters it.
                    let Ok(handle) = (unsafe { DwmRegisterThumbnail(dest, *source) }) else {
                        continue;
                    };
                    entries.insert(key, handle);
                    handle
                }
            };

            let props = DWM_THUMBNAIL_PROPERTIES {
                dwFlags: DWM_TNP_RECTDESTINATION
                    | DWM_TNP_VISIBLE
                    | DWM_TNP_SOURCECLIENTAREAONLY
                    | DWM_TNP_OPACITY,
                rcDestination: *rect,
                fVisible: true.into(),
                // Client area only: the source's own title bar and border
                // would otherwise appear inside a preview that already
                // sits in the overview's own card chrome.
                fSourceClientAreaOnly: true.into(),
                opacity: 255,
                ..Default::default()
            };
            // SAFETY: `handle` came from a successful registration above
            // and has not been unregistered; `props` is a local that
            // outlives the call.
            unsafe {
                let _ = DwmUpdateThumbnailProperties(handle, &props);
            }
        }

        // Anything no longer in `items` (window closed, moved workspace,
        // scrolled out) loses its registration, or DWM would keep
        // compositing it at its last rect.
        let wanted: Vec<isize> = items.iter().map(|(h, _)| h.0 as isize).collect();
        let stale: Vec<isize> =
            entries.keys().copied().filter(|k| !wanted.contains(k)).collect();
        for key in stale {
            if let Some(handle) = entries.remove(&key) {
                // SAFETY: the handle was registered by this module and is
                // removed from the registry in the same step.
                unsafe {
                    let _ = DwmUnregisterThumbnail(handle);
                }
            }
        }
    });
}

/// Drops every live preview for `dest`. Called when the overview closes,
/// and when its window is destroyed — leaving registrations alive would
/// keep DWM compositing previews over a window that is no longer showing
/// them.
pub(crate) fn clear(dest: HWND) {
    REGISTRY.with(|registry| {
        let Some(entries) = registry.borrow_mut().remove(&(dest.0 as isize)) else {
            return;
        };
        for handle in entries.into_values() {
            // SAFETY: as in `sync`'s stale path.
            unsafe {
                let _ = DwmUnregisterThumbnail(handle);
            }
        }
    });
}
