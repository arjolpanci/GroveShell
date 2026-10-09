# Settings App Native Redesign — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Rebuild `groveshell-settings`'s window against the shell's own
native design system, so it reads as a Windows 11 settings app instead of
an invented one.

**Architecture:** Extract the shell's design tokens, icon set, Direct2D
device layer and `Canvas` painter from `apps/ui` into a new workspace
crate `crates/ui-kit`, consumed by both binaries. Add a declarative row/card
layout engine to that crate: a page declares a list of rows, one layout
pass assigns rectangles, and paint, hit-testing and keyboard traversal all
read those same rectangles. Port the settings pages onto it.

**Tech Stack:** Rust 2021, `windows` 0.58 (Win32, Direct2D,
DirectComposition, DWM), owner-drawn rendering through the existing
`Canvas` trait. No new third-party dependencies.

**Spec:** `docs/superpowers/specs/2026-10-09-settings-app-native-redesign.md`

## Global Constraints

- Workspace crate naming follows the existing convention: directory
  `crates/ui-kit`, package name `groveshell-ui-kit`, declared in the root
  `Cargo.toml` `[workspace.dependencies]` as
  `groveshell-ui-kit = { path = "crates/ui-kit" }`.
- No new third-party dependencies. The kit's `windows` features are the
  union of what the moved modules already use.
- Every moved item keeps its existing doc comment and its existing
  `// SAFETY:` comment verbatim. Moving code is not an excuse to drop the
  reasoning attached to it.
- Logical (96-DPI) pixels everywhere in layout code; DPI scaling happens
  through `runtime::scaled(v, dpi)` at the point of use, as the shell
  already does.
- `apps/ui` must build, pass its tests, and run visibly unchanged after
  every extraction task. The shell is working software; this plan borrows
  its code, it does not get to break it.
- Metrics (row heights, radii, gutters) are measured from the reference
  captures named in spec §2.1 during Task 5, not guessed.

## Review Focus

Five conditions the spec implies that no task's happy path exercises.
Each has a test pinned to the task that owns the code.

1. **High contrast mode** — tokens must stay legible, not merely
   different: text and both surface tokens stay at opposite ends in every
   theme. (Task 7)
2. **A window narrower than its content** — rows must clamp, never
   overlap or push controls past the right edge; there is a minimum width
   below which the window refuses to shrink. (Task 5)
3. **Live theme change while the window is open** — a `WM_SETTINGCHANGE`
   mid-session repaints with the new tokens instead of keeping stale ones.
   (Task 7)
4. **More rows than fit** — the content column scrolls, the scroll offset
   clamps at both ends, and hit-testing accounts for the offset so clicks
   land on the row the user sees. (Task 6)
5. **A description too long for its row** — text is clipped with an
   ellipsis at the control's left edge rather than drawn under the
   control. (Task 6)

---

### Task 1: The `groveshell-ui-kit` crate and its runtime mirrors

The kit needs a home for the process-wide values the design tokens read:
the live accent color, the light/dark flag, high contrast, the animation
config, and DPI scaling. These live in `apps/ui/src/imp/state.rs` today as
thread-local `Cell`s; the kit gets its own copy and `apps/ui` forwards to
it, so there is exactly one source of truth in the process.

**Files:**
- Create: `crates/ui-kit/Cargo.toml`
- Create: `crates/ui-kit/src/lib.rs`
- Create: `crates/ui-kit/src/runtime.rs`
- Modify: `Cargo.toml` (workspace dependencies)

**Interfaces:**
- Consumes: nothing.
- Produces: `groveshell_ui_kit::runtime` with
  `scaled(v: i32, dpi: u32) -> i32`,
  `accent() -> u32`, `set_accent(u32)`,
  `light_theme() -> bool`, `set_light_theme(bool)`,
  `high_contrast() -> bool`, `set_high_contrast(bool)`,
  `animation_config() -> (f32, bool)`, `set_animation_config(f32, bool)`.

- [ ] **Step 1: Create the crate manifest**

`crates/ui-kit/Cargo.toml`:

```toml
[package]
name = "groveshell-ui-kit"
version.workspace = true
edition.workspace = true
license.workspace = true
publish.workspace = true

[dependencies]
tracing = { workspace = true }

[target.'cfg(windows)'.dependencies]
windows = { workspace = true, features = [
  "Win32_Foundation",
  "Win32_Graphics_Direct2D",
  "Win32_Graphics_Direct2D_Common",
  "Win32_Graphics_Direct3D",
  "Win32_Graphics_Direct3D11",
  "Win32_Graphics_DirectComposition",
  "Win32_Graphics_Dwm",
  "Win32_Graphics_Dxgi",
  "Win32_Graphics_Dxgi_Common",
  "Win32_Graphics_DirectWrite",
  "Win32_Graphics_Gdi",
  "Win32_System_Registry",
  "Win32_UI_HiDpi",
  "Win32_UI_WindowsAndMessaging",
] }
```

Add to the root `Cargo.toml` under `[workspace.dependencies]`, keeping the
existing alphabetical grouping with the other `groveshell-*` entries:

```toml
groveshell-ui-kit = { path = "crates/ui-kit" }
```

- [ ] **Step 2: Write the failing tests**

`crates/ui-kit/src/runtime.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scaled_rounds_to_nearest_at_fractional_dpi() {
        assert_eq!(scaled(24, 96), 24);
        assert_eq!(scaled(24, 144), 36);
        assert_eq!(scaled(15, 120), 19);
    }

    #[test]
    fn accent_round_trips_through_the_mirror() {
        set_accent(0x00B1_6300);
        assert_eq!(accent(), 0x00B1_6300);
    }

    #[test]
    fn theme_and_contrast_flags_round_trip() {
        set_light_theme(true);
        assert!(light_theme());
        set_light_theme(false);
        assert!(!light_theme());
        set_high_contrast(true);
        assert!(high_contrast());
        set_high_contrast(false);
    }

    #[test]
    fn animation_config_round_trips() {
        set_animation_config(0.5, true);
        assert_eq!(animation_config(), (0.5, true));
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p groveshell-ui-kit`
Expected: FAIL — the crate has no `runtime` module yet.

- [ ] **Step 4: Implement the module**

`crates/ui-kit/src/lib.rs`:

```rust
//! The GroveShell UI kit: the design tokens, icon set, Direct2D device
//! layer and painter shared by `apps/ui` and `apps/settings`.
//!
//! This exists because both binaries draw the same shell: the settings
//! window is not a different product with a different look, and the one
//! time it had its own palette it drifted into something that matched
//! neither Windows nor the shell.

pub mod runtime;
```

`crates/ui-kit/src/runtime.rs` (above the test module):

```rust
//! Process-wide values the design tokens read: the live Windows accent,
//! the light/dark and high-contrast flags, the animation config, and DPI
//! scaling.
//!
//! Thread-local `Cell`s rather than a struct threaded through every
//! call: these are read from inside paint paths that already hold other
//! borrows, and a nested `RefCell` borrow panics (see
//! `apps/ui/src/imp/state.rs`, where this pattern started).

use std::cell::Cell;

thread_local! {
    static ACCENT: Cell<u32> = const { Cell::new(0x00B1_6300) };
    static LIGHT_THEME: Cell<bool> = const { Cell::new(false) };
    static HIGH_CONTRAST: Cell<bool> = const { Cell::new(false) };
    static ANIMATION_SCALE: Cell<f32> = const { Cell::new(1.0) };
    static REDUCED_MOTION: Cell<bool> = const { Cell::new(false) };
}

/// Scales a logical (96-DPI reference) pixel value to `dpi`, rounding to
/// nearest.
pub fn scaled(v: i32, dpi: u32) -> i32 {
    (v * dpi as i32 + 48) / 96
}

pub fn accent() -> u32 {
    ACCENT.with(|c| c.get())
}

pub fn set_accent(value: u32) {
    ACCENT.with(|c| c.set(value));
}

pub fn light_theme() -> bool {
    LIGHT_THEME.with(|c| c.get())
}

pub fn set_light_theme(light: bool) {
    LIGHT_THEME.with(|c| c.set(light));
}

pub fn high_contrast() -> bool {
    HIGH_CONTRAST.with(|c| c.get())
}

pub fn set_high_contrast(on: bool) {
    HIGH_CONTRAST.with(|c| c.set(on));
}

/// `(animation_scale, reduced_motion)` — the pair `design::motion` needs.
pub fn animation_config() -> (f32, bool) {
    (ANIMATION_SCALE.with(|c| c.get()), REDUCED_MOTION.with(|c| c.get()))
}

pub fn set_animation_config(scale: f32, reduced: bool) {
    ANIMATION_SCALE.with(|c| c.set(scale));
    REDUCED_MOTION.with(|c| c.set(reduced));
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p groveshell-ui-kit`
Expected: PASS, 4 tests.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock crates/ui-kit
git commit -m "feat(ui-kit): add the shared UI kit crate and its runtime mirrors"
```

---

### Task 2: Move the design tokens into the kit

`apps/ui/src/imp/design/` becomes `crates/ui-kit/src/design/`. The 114
call sites in `apps/ui` keep working unchanged, because `imp/mod.rs`
re-exports the kit's module under the same name — `super::design::color::x()`
resolves through a `use` in the parent module.

**Files:**
- Create: `crates/ui-kit/src/design/{mod,color,material,metrics,motion,typography}.rs`
  (moved from `apps/ui/src/imp/design/`)
- Create: `crates/ui-kit/src/theme.rs` (the registry read `color::refresh_theme` needs)
- Delete: `apps/ui/src/imp/design/` (whole directory)
- Modify: `apps/ui/src/imp/mod.rs` (re-export), `apps/ui/src/imp/state.rs`
  (delegate mirrors), `apps/ui/src/imp/theme.rs` (delegate the registry read),
  `apps/ui/Cargo.toml` (depend on the kit)

**Interfaces:**
- Consumes: `groveshell_ui_kit::runtime` (Task 1).
- Produces: `groveshell_ui_kit::design::{color, material, metrics, motion, typography}`,
  all items `pub` instead of `pub(crate)`; `groveshell_ui_kit::theme::apps_use_light_theme() -> Option<bool>`.

- [ ] **Step 1: Move the files**

```bash
git mv apps/ui/src/imp/design crates/ui-kit/src/design
```

In the moved files, replace every `pub(crate)` with `pub` and rewrite the
three cross-module references:

- `design/color.rs`, `design/material.rs`, `design/motion.rs`:
  `use super::super::state;` becomes `use crate::runtime as state;`
  (aliasing keeps every `state::light_theme()` call site in those files
  unchanged).
- `design/color.rs:173`: `super::super::theme::apps_use_light_theme()`
  becomes `crate::theme::apps_use_light_theme()`.

Add to `crates/ui-kit/src/lib.rs`:

```rust
pub mod design;
pub mod theme;
```

- [ ] **Step 2: Move the light-theme registry read**

Move `apps_use_light_theme` from `apps/ui/src/imp/theme.rs` into
`crates/ui-kit/src/theme.rs` as `pub fn`, keeping its doc comment. In
`apps/ui/src/imp/theme.rs`, replace the body with a delegation so the
shell's own `theme::toggle_theme` keeps working:

```rust
/// Re-exported from the UI kit, which owns the registry read now that
/// the design tokens there need it too.
pub(crate) use groveshell_ui_kit::theme::apps_use_light_theme;
```

- [ ] **Step 3: Delegate the state mirrors**

In `apps/ui/src/imp/state.rs`, replace the bodies of the mirror
accessors so the kit's cells are the only storage. For example:

```rust
pub(crate) fn scaled(v: i32, dpi: u32) -> i32 {
    groveshell_ui_kit::runtime::scaled(v, dpi)
}

pub(crate) fn light_theme() -> bool {
    groveshell_ui_kit::runtime::light_theme()
}

pub(crate) fn set_light_theme(light: bool) {
    groveshell_ui_kit::runtime::set_light_theme(light);
}
```

Do the same for `accent`/`set_accent`, `high_contrast`/`set_high_contrast`,
and `animation_config`/its setter, and delete the thread-local `Cell`s they
used to read. Leave every other item in `state.rs` alone.

- [ ] **Step 4: Add the dependency and the re-export**

`apps/ui/Cargo.toml`, with the other `groveshell-*` dependencies:

```toml
groveshell-ui-kit = { workspace = true }
```

`apps/ui/src/imp/mod.rs`, beside the other `use` statements at the top:

```rust
/// The design tokens live in the shared kit now (`apps/settings` draws
/// from the same ones). Re-exported under the name every module in this
/// binary already calls them by, so `super::design::color::text()` keeps
/// resolving.
pub(crate) use groveshell_ui_kit::design;
```

Delete the `mod design;` declaration it replaces.

- [ ] **Step 5: Build and run the shell's tests**

Run: `cargo test -p groveshell-ui -p groveshell-ui-kit`
Expected: PASS — the same 125 `groveshell-ui` tests as before plus the
kit's, with the design-token tests now reported under the kit.

- [ ] **Step 6: Verify the shell still looks right**

Stop any running shell, rebuild, start host and ui, and capture the bar
and Quick Settings. Compare against the shell before this task: same
colors, same Mica, same accent. An extraction that changes pixels has a
bug in it.

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "refactor(ui-kit): move the design tokens out of apps/ui"
```

---

### Task 3: Move the icon set and the GDI text helpers

**Files:**
- Create: `crates/ui-kit/src/icons.rs` (moved from `apps/ui/src/imp/icons.rs`)
- Create: `crates/ui-kit/src/text.rs` (`ui_font` and `draw_text_in`, moved
  from `apps/ui/src/imp/util.rs`)
- Modify: `apps/ui/src/imp/mod.rs`, `apps/ui/src/imp/util.rs`

**Interfaces:**
- Consumes: `groveshell_ui_kit::design::typography` (Task 2).
- Produces: `groveshell_ui_kit::icons::{Icon, fluent_glyph, volume_icon,
  battery_icon, draw_icon, draw_fluent_glyph}`,
  `groveshell_ui_kit::text::{ui_font, draw_text_in}`, and
  `groveshell_ui_kit::glyph` (named Segoe Fluent Icons code points), all `pub`.

- [ ] **Step 1: Move the icon module**

```bash
git mv apps/ui/src/imp/icons.rs crates/ui-kit/src/icons.rs
```

Change `pub(crate)` to `pub` throughout and rewrite its three
`super::design::typography::…` references to `crate::design::typography::…`.
Add `pub mod icons;` to `lib.rs`.

- [ ] **Step 2: Move the two GDI text helpers**

Move `ui_font` and `draw_text_in` from `apps/ui/src/imp/util.rs` into a new
`crates/ui-kit/src/text.rs` as `pub fn`, keeping their doc and SAFETY
comments. Add `pub mod text;` to `lib.rs`. In `apps/ui/src/imp/util.rs`,
re-export them so the shell's existing call sites are untouched:

```rust
pub(crate) use groveshell_ui_kit::text::{draw_text_in, ui_font};
```

- [ ] **Step 3: Add named glyph constants**

The settings rows refer to icons by name, not by code point. Create
`crates/ui-kit/src/glyph.rs` and add `pub mod glyph;` to `lib.rs`:

```rust
//! Named Segoe Fluent Icons code points, so row declarations read as
//! `glyph::RESIZE` rather than a bare escape. Each was checked against
//! the font before being added here.

pub const RESIZE: &str = "\u{E740}";
pub const BACKDROP: &str = "\u{E890}";
pub const DOCK: &str = "\u{E8FD}";
pub const OVERVIEW: &str = "\u{E7C4}";
pub const WORKSPACES: &str = "\u{E737}";
pub const INPUT: &str = "\u{E765}";
pub const ACCESSIBILITY: &str = "\u{E776}";
pub const HOME: &str = "\u{E80F}";
pub const ABOUT: &str = "\u{E946}";
pub const STARTUP: &str = "\u{E7E8}";
pub const CHEVRON_DOWN: &str = "\u{E70D}";
pub const CHEVRON_RIGHT: &str = "\u{E76C}";
pub const OK: &str = "\u{E73E}";
pub const WARNING: &str = "\u{E7BA}";
pub const ERROR: &str = "\u{EA39}";
```

Check each code point renders as the intended icon before moving on: a
wrong one is invisible in code review and obvious on screen.

- [ ] **Step 4: Re-export icons from the shell's module tree**

In `apps/ui/src/imp/mod.rs`, replace `mod icons;` with:

```rust
pub(crate) use groveshell_ui_kit::icons;
```

- [ ] **Step 5: Build and test**

Run: `cargo test -p groveshell-ui -p groveshell-ui-kit`
Expected: PASS. The icon tests move to the kit and must still pass there.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "refactor(ui-kit): move the icon set and GDI text helpers out of apps/ui"
```

---

### Task 4: Move the Direct2D device layer and the painter

`gpu.rs` has no `super::` references at all — it is already a standalone
device/surface layer and moves verbatim. `canvas.rs` moves with it and its
`super::gpu` / `super::icons` / `super::util` / `super::state::scaled`
references become `crate::` ones.

**Files:**
- Create: `crates/ui-kit/src/gpu.rs` (moved), `crates/ui-kit/src/canvas.rs` (moved)
- Modify: `apps/ui/src/imp/mod.rs`

**Interfaces:**
- Consumes: Tasks 1–3.
- Produces: `groveshell_ui_kit::gpu::{GpuContext, GpuSurface, init,
  is_enabled, create_surface, create_child_surface, set_opacity, commit,
  redraw, …}` and `groveshell_ui_kit::canvas::{Canvas, GdiCanvas, D2DCanvas}`,
  all `pub`.

- [ ] **Step 1: Move both files**

```bash
git mv apps/ui/src/imp/gpu.rs crates/ui-kit/src/gpu.rs
git mv apps/ui/src/imp/canvas.rs crates/ui-kit/src/canvas.rs
```

Change `pub(crate)` to `pub` in both. In `canvas.rs` rewrite:
`super::gpu::` → `crate::gpu::`, `super::icons::` → `crate::icons::`,
`super::util::` → `crate::text::`, `super::design::` → `crate::design::`,
and `super::state::scaled(…)` → `crate::runtime::scaled(…)`.

Add `pub mod canvas;` and `pub mod gpu;` to `lib.rs`.

- [ ] **Step 2: Re-export both from the shell's module tree**

In `apps/ui/src/imp/mod.rs`, replace `mod gpu;` and `mod canvas;` with:

```rust
pub(crate) use groveshell_ui_kit::{canvas, gpu};
```

- [ ] **Step 3: Build and test**

Run: `cargo test -p groveshell-ui -p groveshell-ui-kit`
Expected: PASS.

- [ ] **Step 4: Verify the shell renders identically**

Start the shell and capture the bar, Quick Settings and the calendar. The
Direct2D path is the one that can break silently — a surface that fails to
create falls back to GDI and still "works", just opaque. Check the bar is
still translucent over a window.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "refactor(ui-kit): move the Direct2D device layer and painter out of apps/ui"
```

---

### Task 5: The row model and layout engine

A page declares rows; this assigns rectangles. Pure arithmetic, no Win32,
fully testable.

**Files:**
- Create: `crates/ui-kit/src/rows/mod.rs`, `crates/ui-kit/src/rows/layout.rs`

**Interfaces:**
- Consumes: `runtime::scaled`, `design::metrics`.
- Produces:

```rust
pub enum Control {
    Toggle { on: bool },
    Slider { value: f32, min: f32, max: f32, unit: &'static str },
    Choice { options: &'static [&'static str], selected: usize },
    Action { label: &'static str },
    Link,
    Status { severity: Severity },
    None,
}

pub enum Severity { Ok, Warning, Error }

pub struct Row {
    pub id: u32,
    pub glyph: Option<&'static str>,
    pub title: String,
    pub description: Option<String>,
    pub control: Control,
    pub enabled: bool,
}

pub struct Card { pub caption: Option<String>, pub rows: Vec<Row> }

pub struct RowRects {
    pub row: RECT,
    pub glyph: RECT,
    pub text: RECT,
    pub control: RECT,
}

pub struct PageLayout {
    pub cards: Vec<(RECT, Vec<RowRects>)>,
    pub content_height: i32,
}

pub fn layout_page(cards: &[Card], content: RECT, dpi: u32) -> PageLayout;
pub const MIN_CONTENT_WIDTH: i32 = 480;
```

- [ ] **Step 1: Measure the reference metrics**

Before writing numbers, open the spec §2.1 captures and measure: card
corner radius, single-line and two-line row heights, the icon gutter
width, the gap between cards in a group and between groups, and the
content column's left inset and maximum width. Record them as named
constants at the top of `layout.rs` with a comment saying they were
measured, not guessed.

- [ ] **Step 2: Write the failing tests**

In `crates/ui-kit/src/rows/layout.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: u32, description: Option<&str>) -> Row {
        Row {
            id,
            glyph: Some("\u{E700}"),
            title: format!("Row {id}"),
            description: description.map(|d| d.to_string()),
            control: Control::Toggle { on: false },
            enabled: true,
        }
    }

    fn content() -> RECT {
        RECT { left: 0, top: 0, right: 900, bottom: 600 }
    }

    #[test]
    fn rows_stack_in_order_without_overlapping() {
        let cards = vec![Card { caption: None, rows: vec![row(1, None), row(2, None), row(3, None)] }];
        let layout = layout_page(&cards, content(), 96);
        let rects = &layout.cards[0].1;
        for pair in rects.windows(2) {
            assert!(pair[0].row.bottom <= pair[1].row.top, "rows overlap: {:?} then {:?}", pair[0].row, pair[1].row);
        }
    }

    #[test]
    fn a_two_line_row_is_taller_than_a_one_line_row() {
        let cards = vec![Card { caption: None, rows: vec![row(1, None), row(2, Some("A description"))] }];
        let layout = layout_page(&cards, content(), 96);
        let rects = &layout.cards[0].1;
        let one_line = rects[0].row.bottom - rects[0].row.top;
        let two_line = rects[1].row.bottom - rects[1].row.top;
        assert!(two_line > one_line);
    }

    #[test]
    fn every_part_of_a_row_stays_inside_it() {
        let cards = vec![Card { caption: None, rows: vec![row(1, Some("desc"))] }];
        let layout = layout_page(&cards, content(), 96);
        let r = &layout.cards[0].1[0];
        for part in [r.glyph, r.text, r.control] {
            assert!(part.left >= r.row.left && part.right <= r.row.right, "{part:?} escapes {:?}", r.row);
            assert!(part.top >= r.row.top && part.bottom <= r.row.bottom);
        }
    }

    #[test]
    fn text_never_runs_under_the_control() {
        let cards = vec![Card { caption: None, rows: vec![row(1, Some("a very long description that would happily run the whole width of the row if nothing stopped it"))] }];
        let layout = layout_page(&cards, content(), 96);
        let r = &layout.cards[0].1[0];
        assert!(r.text.right <= r.control.left);
    }

    // Review Focus 2: a window narrower than its content.
    #[test]
    fn a_narrow_content_area_still_produces_a_sane_row() {
        let narrow = RECT { left: 0, top: 0, right: MIN_CONTENT_WIDTH, bottom: 600 };
        let cards = vec![Card { caption: None, rows: vec![row(1, Some("desc"))] }];
        let layout = layout_page(&cards, narrow, 96);
        let r = &layout.cards[0].1[0];
        assert!(r.text.right <= r.control.left, "text and control collided at the minimum width");
        assert!(r.control.right <= narrow.right);
        assert!(r.text.left < r.text.right, "text column collapsed to nothing");
    }

    #[test]
    fn the_content_column_stops_growing_on_a_very_wide_window() {
        let wide = RECT { left: 0, top: 0, right: 4000, bottom: 600 };
        let cards = vec![Card { caption: None, rows: vec![row(1, None)] }];
        let layout = layout_page(&cards, wide, 96);
        let card = layout.cards[0].0;
        assert!(card.right - card.left <= MAX_CONTENT_WIDTH);
    }

    #[test]
    fn layout_scales_with_dpi() {
        let cards = vec![Card { caption: None, rows: vec![row(1, None)] }];
        let at96 = layout_page(&cards, content(), 96);
        let at192 = layout_page(&cards, content(), 192);
        let h96 = at96.cards[0].1[0].row.bottom - at96.cards[0].1[0].row.top;
        let h192 = at192.cards[0].1[0].row.bottom - at192.cards[0].1[0].row.top;
        assert_eq!(h192, h96 * 2);
    }

    #[test]
    fn content_height_covers_every_card() {
        let cards = vec![
            Card { caption: Some("One".into()), rows: vec![row(1, None), row(2, None)] },
            Card { caption: Some("Two".into()), rows: vec![row(3, None)] },
        ];
        let layout = layout_page(&cards, content(), 96);
        let last = layout.cards.last().unwrap().0;
        assert!(layout.content_height >= last.bottom - content().top);
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p groveshell-ui-kit rows::`
Expected: FAIL — `layout_page` does not exist.

- [ ] **Step 4: Implement the model and the layout pass**

Write `rows/mod.rs` with the types from the Interfaces block (deriving
`Clone` and `Debug`; `Row::new(id, title)` plus builder-style
`with_description`, `with_glyph`, `with_control` keep the page
declarations readable). Write `layout_page` in `rows/layout.rs`:

- Centre a content column of `min(content width, MAX_CONTENT_WIDTH)`.
- For each card: optional caption line above it, then rows stacked at
  single- or two-line height depending on `description`.
- Within a row: glyph gutter on the left, control sized by its kind and
  right-aligned, text column filling what is left — clamped so
  `text.right <= control.left` always holds, including at
  `MIN_CONTENT_WIDTH`.
- Scale every constant through `runtime::scaled(v, dpi)`.
- `content_height` is the bottom of the last card plus the trailing margin.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p groveshell-ui-kit rows::`
Expected: PASS, 8 tests.

- [ ] **Step 6: Commit**

```bash
git add crates/ui-kit/src/rows
git commit -m "feat(ui-kit): add the settings row model and layout engine"
```

---

### Task 6: Painting, hit-testing, scrolling and focus

The three consumers of the layout from Task 5. Hit-testing and focus are
pure and get tests; painting is verified by looking at it.

**Files:**
- Create: `crates/ui-kit/src/rows/paint.rs`, `crates/ui-kit/src/rows/input.rs`

**Interfaces:**
- Consumes: `PageLayout`, `Card`, `Control` (Task 5); `Canvas` (Task 4).
- Produces:

```rust
pub fn paint_page(canvas: &mut dyn Canvas, cards: &[Card], layout: &PageLayout, focused: Option<u32>, hovered: Option<u32>, scroll: i32, dpi: u32);

pub enum Hit { Row(u32), SliderDrag { id: u32, value: f32 }, None }
pub fn hit_test(cards: &[Card], layout: &PageLayout, x: i32, y: i32, scroll: i32) -> Hit;

pub fn clamp_scroll(scroll: i32, content_height: i32, viewport_height: i32) -> i32;
pub fn next_focus(cards: &[Card], current: Option<u32>, forward: bool) -> Option<u32>;
```

- [ ] **Step 1: Write the failing tests**

In `crates/ui-kit/src/rows/input.rs` (reusing the `row`/`content` helpers
from Task 5's tests — copy them into this module's test block):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_click_inside_a_row_hits_that_row() {
        let cards = vec![Card { caption: None, rows: vec![row(7, None)] }];
        let layout = layout_page(&cards, content(), 96);
        let r = layout.cards[0].1[0].row;
        assert!(matches!(hit_test(&cards, &layout, r.left + 10, (r.top + r.bottom) / 2, 0), Hit::Row(7)));
    }

    #[test]
    fn a_click_between_cards_hits_nothing() {
        let cards = vec![
            Card { caption: None, rows: vec![row(1, None)] },
            Card { caption: Some("Second".into()), rows: vec![row(2, None)] },
        ];
        let layout = layout_page(&cards, content(), 96);
        let gap_y = layout.cards[0].0.bottom + 1;
        assert!(matches!(hit_test(&cards, &layout, 100, gap_y, 0), Hit::None));
    }

    // Review Focus 4: hit-testing must account for the scroll offset.
    #[test]
    fn hit_testing_follows_the_scroll_offset() {
        let cards = vec![Card { caption: None, rows: vec![row(1, None), row(2, None)] }];
        let layout = layout_page(&cards, content(), 96);
        let second = layout.cards[0].1[1].row;
        let scroll = 40;
        let on_screen_y = (second.top + second.bottom) / 2 - scroll;
        assert!(matches!(hit_test(&cards, &layout, 100, on_screen_y, scroll), Hit::Row(2)));
    }

    #[test]
    fn a_disabled_row_is_not_hit() {
        let mut r = row(5, None);
        r.enabled = false;
        let cards = vec![Card { caption: None, rows: vec![r] }];
        let layout = layout_page(&cards, content(), 96);
        let rect = layout.cards[0].1[0].row;
        assert!(matches!(hit_test(&cards, &layout, rect.left + 10, (rect.top + rect.bottom) / 2, 0), Hit::None));
    }

    #[test]
    fn scroll_clamps_at_both_ends() {
        assert_eq!(clamp_scroll(-50, 1000, 400), 0);
        assert_eq!(clamp_scroll(5000, 1000, 400), 600);
        assert_eq!(clamp_scroll(100, 1000, 400), 100);
    }

    #[test]
    fn content_shorter_than_the_viewport_never_scrolls() {
        assert_eq!(clamp_scroll(80, 200, 400), 0);
    }

    #[test]
    fn tab_order_matches_visual_order_and_wraps() {
        let cards = vec![
            Card { caption: None, rows: vec![row(1, None), row(2, None)] },
            Card { caption: None, rows: vec![row(3, None)] },
        ];
        assert_eq!(next_focus(&cards, None, true), Some(1));
        assert_eq!(next_focus(&cards, Some(1), true), Some(2));
        assert_eq!(next_focus(&cards, Some(3), true), Some(1));
        assert_eq!(next_focus(&cards, Some(1), false), Some(3));
    }

    #[test]
    fn focus_skips_a_disabled_row() {
        let mut middle = row(2, None);
        middle.enabled = false;
        let cards = vec![Card { caption: None, rows: vec![row(1, None), middle, row(3, None)] }];
        assert_eq!(next_focus(&cards, Some(1), true), Some(3));
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p groveshell-ui-kit rows::input`
Expected: FAIL — `hit_test` does not exist.

- [ ] **Step 3: Implement input**

`hit_test` offsets `y` by `scroll`, walks the cards' row rects, returns
`Hit::None` for a disabled row, and returns `Hit::SliderDrag` with the
value computed from `x` within the control rect when the row's control is
a `Slider`. `clamp_scroll` clamps to `0..=(content_height - viewport_height).max(0)`.
`next_focus` flattens the cards' rows into one order, filters out disabled
rows, and steps with wraparound.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p groveshell-ui-kit rows::input`
Expected: PASS, 8 tests.

- [ ] **Step 5: Implement painting**

`paint_page` draws, per card: the caption, the card's rounded fill using
`design::color` tokens, a hairline divider between adjacent rows, then per
row — the glyph in `typography::ICON_FACE`, the title at `BODY_PX`, the
description at `CAPTION_PX` in the muted token, and the control.

Controls: a toggle draws its `On`/`Off` label to the left of the pill, the
pill filled with `design::color::accent()` when on; a slider draws track,
accent-filled portion, thumb and its value label; a choice draws a
bordered box with the selected option and a chevron glyph; an action
draws a bordered button; a link draws its title in the accent color; a
status draws a severity glyph and two lines.

Hover tints the row background, focus strokes the system focus rectangle
around it, and a disabled row draws every part in the muted token.

**Review Focus 5:** the title and description are drawn into `RowRects::text`,
which Task 5 guarantees ends before the control. Pass `DT_END_ELLIPSIS`
so a string too long for that rect is ellipsised rather than drawn under
the control.

- [ ] **Step 6: Commit**

```bash
git add crates/ui-kit/src/rows
git commit -m "feat(ui-kit): paint, hit-test, scroll and focus settings rows"
```

---

### Task 7: The settings window shell

**Files:**
- Modify: `apps/settings/src/imp/window.rs` (substantially rewritten)
- Modify: `apps/settings/Cargo.toml` (depend on the kit)
- Create: `apps/settings/src/imp/surface.rs` (the window's DComp surface)

**Interfaces:**
- Consumes: `gpu::{init, create_surface, redraw, commit}`, `canvas::D2DCanvas`,
  `design::material`, `rows::*`.
- Produces: a window that hosts a `&[Card]` and repaints on demand.

- [ ] **Step 1: Depend on the kit**

`apps/settings/Cargo.toml`: add `groveshell-ui-kit = { workspace = true }`
and the `Win32_Graphics_Direct2D`/`DirectComposition`/`Dxgi` features the
kit's surface calls need from this binary's own `windows` dependency.

- [ ] **Step 2: Create the window's surface**

`apps/settings/src/imp/surface.rs`: on `WM_CREATE`, call
`gpu::init()` then `gpu::create_surface(hwnd, width, height)`; on
`WM_SIZE`, recreate it at the new size; paint by calling `gpu::redraw`
with a closure that builds a `D2DCanvas` and calls `rows::paint_page`,
then `gpu::commit()`. Fall back to `GdiCanvas` when `gpu::is_enabled()`
is false, exactly as `apps/ui`'s surfaces do.

Create the window with `WS_EX_NOREDIRECTIONBITMAP` so the backdrop can
show through, and clear to alpha 0 before drawing — the two conditions
the shell learned are required for a translucent surface.

- [ ] **Step 3: Apply the native window attributes**

`design::material::Surface` has exactly two variants today, `Bar` and
`Flyout`; neither describes an ordinary app window (the bar deliberately
suppresses rounded corners, which a normal window wants). Add a third in
the kit:

```rust
    /// An ordinary application window: Mica, rounded corners, and a
    /// title bar tinted to match the system theme. The settings window.
    Window,
```

and extend `backdrop_for` (`DWMSBT_MAINWINDOW`) and `corner_for`
(`DWMWCP_ROUND`) to cover it. Then, on `WM_CREATE`, call
`design::material::apply(hwnd, Surface::Window)`, and set per-monitor-v2
DPI awareness in `main` before any window exists.

- [ ] **Step 4: Handle resize, DPI and scroll**

- `WM_GETMINMAXINFO`: refuse to shrink below `MIN_CONTENT_WIDTH` plus the
  nav rail, and below a height of 400 logical pixels.
- `WM_SIZE`: recreate the surface, relayout, repaint.
- `WM_DPICHANGED`: adopt the suggested rect, relayout at the new DPI,
  repaint.
- `WM_MOUSEWHEEL`: adjust the scroll offset through `clamp_scroll`, repaint.

- [ ] **Step 5: Follow the system theme live**

**Review Focus 3.** On `WM_SETTINGCHANGE`, call `design::color::refresh_theme()`
and `refresh_accent()` and invalidate the window, the same way
`apps/ui/src/imp/mod.rs` already reacts to the broadcast. Add a test in
`crates/ui-kit/src/design/color.rs` that pins the token contract this
relies on:

```rust
#[test]
fn tokens_differ_between_light_and_dark() {
    assert_ne!(text_for(false, true), text_for(false, false));
    assert_ne!(surface_base_for(false, true), surface_base_for(false, false));
}

// Review Focus 1: high contrast must stay legible, not merely different.
#[test]
fn high_contrast_keeps_text_and_surface_at_opposite_ends() {
    for light in [true, false] {
        assert_ne!(text_for(true, light), surface_base_for(true, light));
        assert_ne!(text_for(true, light), surface_raised_for(true, light));
    }
}
```

The `theme_token!` macro at `color.rs:56` generates both a live form and a
`_for(high_contrast, light)` pure form for each of `surface_base`,
`surface_raised`, `surface_overlay`, `text`, `text_muted` and `stroke`;
the pure forms are what these tests use.

- [ ] **Step 6: Verify by looking at it**

Build, run `groveshell-settings.exe`, capture the window, and compare side
by side with the Windows 11 Settings capture from spec §2.1: Mica
backdrop, correct title-bar tint, resizes, scrolls, and nothing centered.

- [ ] **Step 7: Commit**

```bash
git add -A
git commit -m "feat(settings): render the window with Direct2D, Mica and live theme"
```

---

### Task 8: Port the pages to declared rows

**Files:**
- Modify: `apps/settings/src/imp/pages/{home,dock,top_bar,overview,input,accessibility}.rs`
- Modify: `apps/settings/src/imp/pages/mod.rs` (the `Page` trait)
- Delete: `apps/settings/src/imp/theme.rs`, `apps/settings/src/imp/util_text.rs`

**Interfaces:**
- Consumes: `rows::{Card, Row, Control}`.
- Produces: `trait Page { fn cards(&self) -> Vec<Card>; fn on_activate(&mut self, id: u32); fn on_value(&mut self, id: u32, value: f32); }`

- [ ] **Step 1: Replace the `Page` trait**

`pages/mod.rs`:

```rust
/// A page is a list of cards. Layout, painting, hit-testing and keyboard
/// traversal all come from `groveshell_ui_kit::rows`, so a page never
/// computes a rectangle and the four can never disagree.
pub(crate) trait Page {
    /// The page's content, rebuilt from the current config on each paint.
    fn cards(&self) -> Vec<Card>;
    /// A row was clicked, or activated with Space/Enter.
    fn on_activate(&mut self, id: u32);
    /// A slider or choice row produced a new value.
    fn on_value(&mut self, id: u32, value: f32) {
        let _ = (id, value);
    }
}
```

- [ ] **Step 2: Port the Top Bar page**

```rust
const ROW_HEIGHT: u32 = 1;
const ROW_BLUR: u32 = 2;

impl Page for TopBarPage {
    fn cards(&self) -> Vec<Card> {
        let config = config_store::current();
        vec![Card {
            caption: None,
            rows: vec![
                Row::new(ROW_HEIGHT, "Height")
                    .with_description("How tall the top bar is")
                    .with_glyph(glyph::RESIZE)
                    .with_control(Control::Slider {
                        value: config.appearance.top_bar_height as f32,
                        min: 24.0,
                        max: 48.0,
                        unit: "px",
                    }),
                Row::new(ROW_BLUR, "Blur")
                    .with_description("Show the desktop through the bar")
                    .with_glyph(glyph::BACKDROP)
                    .with_control(Control::Toggle { on: config.appearance.top_bar_blur }),
            ],
        }]
    }

    fn on_activate(&mut self, id: u32) {
        if id == ROW_BLUR {
            let current = config_store::current().appearance.top_bar_blur;
            config_store::update(|c| c.appearance.top_bar_blur = !current);
        }
    }

    fn on_value(&mut self, id: u32, value: f32) {
        if id == ROW_HEIGHT {
            config_store::update(|c| c.appearance.top_bar_height = value.round() as u32);
        }
    }
}
```

- [ ] **Step 3: Port the remaining five pages**

Same shape as Step 2 — `cards()` reads `config_store::current()`,
`on_activate` toggles, `on_value` writes. Each page declares `const` row
ids, never bare literals. The rows, with the exact config field each one
edits:

| Page | Row | Control | Config field |
|---|---|---|---|
| Dock | Mode | Choice: `overview`, `desktop`, `both` | `appearance.dock_mode` |
| Dock | Icon size | Slider 24-64 px | `appearance.dock_icon_size` |
| Dock | Alignment | Choice: `left`, `center`, `right` | `appearance.dock_alignment` |
| Overview | Blur | Toggle | `appearance.overview_blur` |
| Input | Move modifier | Choice: `Alt`, `Super` | `input.move_modifier` |
| Input | Move button | Choice: `Left`, `Right`, `Middle` | `input.move_button` |
| Input | Resize button | Choice: `Left`, `Right`, `Middle` | `input.resize_button` |
| Input | Activities modifier | Choice: `Alt`, `Super` | `input.overview_modifier` |
| Input | Hot corners | One row per corner, Choice of action | `hot_corners` |
| Accessibility | Reduced motion | Toggle | `appearance.reduced_motion` |
| Accessibility | High contrast | Toggle | `appearance.high_contrast` |
| Accessibility | Animation speed | Slider 0.5-2.0 | `appearance.animation_scale` |

The hot-corner rows are the one non-uniform case: `hot_corners` is a
`BTreeMap<String, HotCornerConfig>` keyed by corner name, so the page
builds one row per known corner and `on_value` writes back into the entry
for that corner's key, inserting it if absent.

Home keeps its current information for now, expressed as rows: one status
row per process and an action row carrying the Start/Restore button. Its
redesign is the next plan - do not improve it here.

- [ ] **Step 4: Delete the superseded drawing code**

```bash
git rm apps/settings/src/imp/theme.rs apps/settings/src/imp/util_text.rs
```

Remove their `mod` declarations from `imp/mod.rs`. If anything still
references them, it was not ported — port it rather than keeping the file.

- [ ] **Step 5: Test and look**

Run: `cargo test --workspace`
Expected: PASS.

Then run the app and visit every page. Each control must change its
setting and the shell must react live, which is the behavior
`config_store::update` already provides.

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "feat(settings): declare pages as rows and delete the hand-drawn layout"
```

---

### Task 9: The navigation rail

**Files:**
- Modify: `apps/settings/src/imp/nav.rs`, `apps/settings/src/imp/window.rs`

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn every_nav_item_has_a_glyph() {
    for item in NAV_ITEMS {
        assert!(!item.glyph.is_empty(), "{} has no icon", item.label);
    }
}

#[test]
fn keyboard_navigation_moves_and_stops_at_the_ends() {
    assert_eq!(next_nav(0, 1), 1);
    assert_eq!(next_nav(0, -1), 0);
    assert_eq!(next_nav(NAV_ITEMS.len() - 1, 1), NAV_ITEMS.len() - 1);
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p groveshell-settings nav::`
Expected: FAIL — `NAV_ITEMS` is still `&[&str]` with no glyph field.

- [ ] **Step 3: Implement**

Change `NAV_ITEMS` to a `&[NavItem]` of `{ label: &'static str, glyph:
&'static str }` using Segoe Fluent Icons code points, add `next_nav`, and
draw each item's glyph left of its label through the kit's `Canvas`.
Keep the existing selection pill and left accent bar — they already match
Windows 11 — but take their colors from `design::color` instead of the
deleted literals.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p groveshell-settings nav::`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add -A
git commit -m "feat(settings): give the nav rail icons and keyboard navigation"
```

---

## What this plan does not cover

Spec §3 (the `--background` startup model and first run), §8 (honest
health and the redesigned Home) and the About and Workspaces pages are
the subject of a second plan, written after this one lands. They depend on
this plan's row library but not on each other's internals, and they change
product behavior rather than rendering — a different kind of review.
