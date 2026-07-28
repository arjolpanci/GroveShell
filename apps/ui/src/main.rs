//! `groveshell-ui`: first-iteration shell UI. Per `docs/PROJECT_PLAN.md`
//! §10.1/§10.2, creates **one top bar per monitor** (Phase 4: "Create
//! per-monitor top bar windows and reserve working area") — only the
//! primary monitor's bar carries the three interactive regions (Activities,
//! clock, Quick Settings); other monitors just get an empty reserved strip
//! for now. Three flyouts:
//!
//! - **Activities overview**: spans the full virtual screen (every
//!   monitor), so a window on a non-primary monitor is fully inside its
//!   bounds and clickable. Each currently-connected monitor also gets its
//!   own pinned workspace card (see "Workspaces" below); the overview is a
//!   GNOME-style carousel of those cards (see `imp::overview`).
//! - **Calendar + notifications**: clicking the clock opens a Windows-11-style
//!   flyout centered below it — a real month calendar (today highlighted)
//!   stacked over a notifications section. The notifications section is a
//!   static "No new notifications" placeholder: reading the real
//!   notification feed requires `UserNotificationListener`, which needs a
//!   packaged app identity and explicit user consent that this unpackaged
//!   process doesn't have. Not faked beyond that empty-state text.
//! - **Quick Settings**: clicking the right side opens a flyout below it
//!   with real, working volume control (`IAudioEndpointVolume` — up/down/
//!   mute) and a read-only battery/AC status line. There's no icon tray:
//!   hosting other apps' notification-area icons would mean impersonating
//!   Explorer's own tray window, which is out of scope until Phase 7
//!   (Explorer replacement) per `docs/PROJECT_PLAN.md` §7 and ADR-002.
//!
//! Only one flyout is ever open at a time; opening any of the three closes
//! the other two. Each bar reserves its strip of its own monitor's work
//! area via the AppBar API (`SHAppBarMessage`), the same mechanism the
//! Windows taskbar uses, so maximized windows and desktop icon layout
//! respect it instead of being covered. While this shell runs it also
//! takes over the real Windows taskbar's screen space entirely — see
//! `imp::taskbar`.
//!
//! ## Workspaces (Phase 3 + part of Phase 5)
//!
//! This is `groveshell-window-model`'s workspace tracker (see
//! `groveshell_window_model::workspace`), with each currently-connected
//! *monitor* additionally getting its own fixed, always-present workspace
//! ("pinned"), ordered left-to-right by real screen position, plus the
//! standard GNOME-style dynamic tail (starts at one extra empty workspace,
//! grows/shrinks per §8.3) beyond the monitors. A monitor's own workspace
//! is never really "switched away from" in the hide/show sense — its
//! windows are already, always visible on that real screen — so
//! `imp::workspaces::commit_workspace_switch` only ever parks/unparks
//! windows assigned to the dynamic (non-monitor) tail. "Current" mostly
//! matters for which page the overview centers on and where a moved
//! window ends up.
//!
//! Live window tracking (`SetWinEventHook`, see `imp::workspaces`) keeps
//! the workspace model in sync in the background — a new window, a closed
//! one, or activating a window on another workspace through Alt-Tab all
//! update the shell without needing the overview reopened. A
//! `groveshell_window_model::registry::WindowRegistry` gives every
//! observed window a stable identity across HWND reuse, so a recycled
//! handle never inherits a dead window's workspace assignment.
//!
//! The Activities overview is a GNOME-style *carousel*: one fixed-size card
//! per workspace, wallpaper-filled, laid out side by side so the current
//! one is centered (and slightly larger) with previous/next cards peeking
//! at the edges (see `imp::overview::card_layout`); dragging horizontally
//! slides between them, and a window's preview can itself be dragged onto
//! a different card to move it there. A workspace's open windows are
//! arranged in a simple auto grid *within* its card — not at their real
//! screen position/size — each with a small app-icon badge straddling its
//! bottom edge (see `imp::overview::layout_grid`). Every slot's preview
//! comes from this shell's own `PrintWindow` capture rather than a live
//! DWM thumbnail: windows on an inactive workspace are *parked off-screen*
//! rather than hidden (see `imp::workspaces::park_window`), and many apps
//! stop rendering once fully off-screen, so the capture is taken right
//! before parking and painted in place of a live preview. Global hotkeys
//! (`Ctrl+Alt+Left/Right` to switch, `+Shift` to also move the focused
//! window) work whether or not the overview is open, and so does
//! type-to-search (open windows, then installed Start Menu apps). The top
//! bar stays visually on top of the overview (`SetWindowPos`
//! `HWND_TOPMOST` reassertion — see `imp::overview::open_overview`), and
//! opening/closing combines a whole-window fade
//! (`SetLayeredWindowAttributes`) with a GNOME-style zoom: the current
//! workspace's card starts blown up to roughly monitor size and shrinks
//! into its carousel slot on open, and the selected card grows back out on
//! close.
//!
//! Deliberately out of scope for this slice: *per-monitor sets of virtual*
//! workspaces (every monitor currently shares the one dynamic tail),
//! Windows' own Task View virtual desktops (a separate, unrelated mechanism
//! — DWM already cloaks windows on an inactive one, and this shell's
//! cloaked-window filter already excludes those from every workspace's
//! page), hot corners, and a dock.

#[cfg(windows)]
mod imp;

#[cfg(windows)]
fn main() -> groveshell_common::Result<()> {
    imp::main()
}

#[cfg(not(windows))]
fn main() {
    eprintln!("groveshell-ui is Windows-only.");
    std::process::exit(1);
}
