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
    DWM_THUMBNAIL_PROPERTIES, DWM_TNP_OPACITY, DWM_TNP_RECTDESTINATION, DWM_TNP_RECTSOURCE,
    DWM_TNP_SOURCECLIENTAREAONLY, DWM_TNP_VISIBLE,
};
use windows::Win32::UI::WindowsAndMessaging::{IsIconic, IsWindowVisible};

thread_local! {
    /// destination window -> `(source window, live registration)`, in
    /// the order they were registered. DWM stacks thumbnails in that
    /// order, later ones on top, and offers no way to restack them.
    static REGISTRY: RefCell<HashMap<isize, Vec<(isize, isize)>>> =
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

/// Whether the previews already registered are stacked the way `wanted`
/// asks, bottom first. Sources in only one of the two lists do not
/// count: one that is gone is about to be dropped, and a new one is
/// registered last, which puts it on top.
pub(crate) fn same_stacking(registered: &[isize], wanted: &[isize]) -> bool {
    let kept: Vec<isize> = registered.iter().copied().filter(|r| wanted.contains(r)).collect();
    let asked: Vec<isize> = wanted.iter().copied().filter(|w| registered.contains(w)).collect();
    kept == asked
}

/// Points `dest`'s live previews at exactly `items`, registering what is
/// new, moving what already existed, and dropping what is gone.
///
/// `items` is bottom first: each is the source, where it goes, and which
/// part of the source to show (`None` for all of it). The order matters
/// on the desktop end of the transition, where previews overlap exactly
/// as their windows do and have to be stacked the same way.
///
/// Called every frame the overview paints, so it is written to do nothing
/// but a cheap property update in the common case — registration only
/// happens the first time a given source appears, or when the stacking
/// asked for no longer matches.
pub(crate) fn sync(dest: HWND, items: &[(HWND, RECT, Option<RECT>)]) {
    REGISTRY.with(|registry| {
        let mut registry = registry.borrow_mut();
        let entries = registry.entry(dest.0 as isize).or_default();
        let wanted: Vec<isize> = items.iter().map(|(h, _, _)| h.0 as isize).collect();

        // Anything no longer in `items` (window closed, moved workspace,
        // scrolled out) loses its registration, or DWM would keep
        // compositing it at its last rect. If what is left is stacked
        // wrongly, or a new source belongs underneath an existing one,
        // everything goes and is registered again in order.
        let registered: Vec<isize> = entries.iter().map(|(source, _)| *source).collect();
        let restack = !same_stacking(&registered, &wanted)
            || wanted
                .iter()
                .position(|w| !registered.contains(w))
                .is_some_and(|first_new| wanted[first_new..].iter().any(|w| registered.contains(w)));
        entries.retain(|(source, handle)| {
            let keep = !restack && wanted.contains(source);
            if !keep {
                // SAFETY: the handle was registered by this module and
                // leaves the registry in the same step.
                unsafe {
                    let _ = DwmUnregisterThumbnail(*handle);
                }
            }
            keep
        });

        for (source, rect, part) in items {
            let key = source.0 as isize;
            let handle = match entries.iter().find(|(s, _)| *s == key) {
                Some(&(_, existing)) => existing,
                None => {
                    // SAFETY: both are live top-level windows; the
                    // returned handle is kept in the registry until
                    // `clear` or the sweep above unregisters it.
                    let Ok(handle) = (unsafe { DwmRegisterThumbnail(dest, *source) }) else {
                        continue;
                    };
                    entries.push((key, handle));
                    handle
                }
            };

            let mut props = DWM_THUMBNAIL_PROPERTIES {
                dwFlags: DWM_TNP_RECTDESTINATION
                    | DWM_TNP_VISIBLE
                    | DWM_TNP_SOURCECLIENTAREAONLY
                    | DWM_TNP_OPACITY,
                rcDestination: *rect,
                fVisible: true.into(),
                // The whole window, title bar included. A preview starts
                // out exactly covering its window, and only the client
                // area stretched over that rect is a visibly different
                // picture.
                fSourceClientAreaOnly: false.into(),
                opacity: 255,
                ..Default::default()
            };
            if let Some(part) = part {
                props.dwFlags |= DWM_TNP_RECTSOURCE;
                props.rcSource = *part;
            }
            // SAFETY: `handle` came from a successful registration above
            // and has not been unregistered; `props` is a local that
            // outlives the call.
            unsafe {
                let _ = DwmUpdateThumbnailProperties(handle, &props);
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
        for (_, handle) in entries {
            // SAFETY: as in `sync`'s stale path.
            unsafe {
                let _ = DwmUnregisterThumbnail(handle);
            }
        }
    });
}

/// Whether a still capture is worth taking for `source` when the
/// overview is built.
///
/// A window DWM will preview live needs no still: the live thumbnail is
/// laid over the same rect from the first frame, so the capture is never
/// seen. Taking one anyway cost ~31ms *per window* on the open path, all
/// of it blocking the animation's first frame — which is what made
/// Activities freeze before it appeared.
///
/// Everything else does need one: a window parked on another workspace
/// is hidden and has no live contents, and that still is the only thing
/// that can represent it.
pub(crate) fn needs_still_capture(previewable: bool, has_capture: bool) -> bool {
    !has_capture && !previewable
}

#[cfg(test)]
mod stacking_tests {
    use super::*;

    #[test]
    fn the_same_order_needs_no_restack() {
        assert!(same_stacking(&[1, 2, 3], &[1, 2, 3]));
        assert!(same_stacking(&[], &[1, 2]));
    }

    #[test]
    fn a_dropped_or_added_source_alone_is_not_a_reorder() {
        assert!(same_stacking(&[1, 2, 3], &[1, 3]));
        assert!(same_stacking(&[1, 3], &[1, 3, 4]));
    }

    #[test]
    fn two_sources_swapping_is() {
        assert!(!same_stacking(&[1, 2, 3], &[1, 3, 2]));
        assert!(!same_stacking(&[1, 2], &[2, 1, 5]));
    }
}

#[cfg(test)]
mod capture_tests {
    use super::*;

    #[test]
    fn an_on_screen_window_needs_no_still_because_it_gets_a_live_one() {
        assert!(!needs_still_capture(true, false));
    }

    #[test]
    fn a_parked_or_minimized_window_still_needs_one() {
        assert!(needs_still_capture(false, false));
    }

    #[test]
    fn nothing_is_recaptured_when_a_capture_already_exists() {
        assert!(!needs_still_capture(false, true));
        assert!(!needs_still_capture(true, true));
    }
}
