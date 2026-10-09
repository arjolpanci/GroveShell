# Quick Settings and Calendar — Native Finish

**Date:** 2026-10-09
**Status:** Proposed.
**Relevant ADRs:** ADR-004 (DirectComposition shell UI), ADR-009 (control
center and tray integration), ADR-010 (one painter, two canvas backends).
**Builds on:** `2026-10-09-settings-app-native-redesign.md`, which
extracted `crates/ui-kit` and established the method this applies to two
more surfaces.

## 1. Goal

Give the shell's two flyouts the same native finish the settings window
got: the shared design tokens, a shared widget vocabulary, correct DPI
behavior, and interaction states that mean what they say.

Explicitly **not** in scope: the Activities overview, and any change to
what the flyouts *do*. This is finish work on existing structure —
every page, chip, link and action stays.

## 2. What is wrong today

From captures of the running shell, not from memory.

### 2.1 The calendar is not DPI-scaled at all

`apps/ui/src/imp/calendar.rs` has no `scaled()` call anywhere. Every
coordinate is a raw constant, the window is created at `CAL_WIDTH ×
CAL_HEIGHT`, and `apply_reveal` documents the choice: *"Deliberately
unscaled"*. On a 200% display the flyout is 320×440 physical pixels
where Quick Settings correctly renders 840×792 for its 420×396 logical
layout. It draws at half size — small text, cramped grid, and the
washed-out look that follows from both.

This is the largest single defect in either surface and the reason the
calendar reads as unfinished next to the rest of the shell.

Beyond scaling: today is a bare accent-colored numeral rather than the
filled pill Windows draws; there is no month navigation; and the
notifications area is a flat strip with a bare "Notifications" label
rather than a header row.

### 2.2 Quick Settings' finish, not its structure

The structure is right and stays. The finish is not:

- **Chips are 60px tall with a full-bleed accent fill.** Four saturated
  slabs where Windows 11 uses calmer, shorter chips.
- **A permanent focus ring.** `change_page` sets `i.focus = Some(0)`, so
  every detail page opens with the accent ring drawn around its back
  button whether or not the keyboard is in use. The settings window had
  the same bug; a ring that is always on tells the user nothing.
- **Detail pages open with prose.** "Select speakers or headphones and
  set the volume for individual apps." Windows explains nothing in this
  surface.
- **The Wi-Fi list is a developer view.** Signal strength as "97%"
  instead of a bar glyph; "Set up securely" wrapping onto a second line
  and overflowing into the row beneath; and a literal instruction,
  *"7 nearby networks · scroll or Page Up / Down for more"*.
- **A title Windows does not have** ("Control center"), and dead space
  at the bottom of every page from the fixed 420×396 panel.
- **Ad-hoc bottom links.** "Sound output ›" and "Windows settings ›" at
  inconsistent spacing with a gap between them.

## 3. Approach

Add a `controls` module to `crates/ui-kit` holding the control-center
widget vocabulary, and rebuild both flyouts' render paths against it.

Not the `rows` Card/Row engine: a control center is not a settings list,
and reusing that layout would make the flyout look like a settings page
in a popup. The two modules share tokens, `Canvas`, glyphs and metrics —
which is the part worth sharing — and differ where the surfaces differ.

The widgets:

| Widget | Used by |
|---|---|
| `chip` — icon + label, optional split arrow, on/off/unavailable | Quick Settings home |
| `slider_row` — icon button, track, value label | volume |
| `list_row` — label, optional trailing text, chevron or external-link mark | detail pages, networks |
| `panel_header` — back button, title | every detail page |
| `status_row` — glyph + label | battery |

Each is a pure layout function plus a paint function, the same split the
`rows` module uses, so geometry is testable and painting is verified by
looking.

## 4. Quick Settings

**Home.** Chips drop to 48px logical with the Windows chip proportions:
icon at the leading edge, label beside it, split arrow in its own
trailing segment behind a divider. The on-state keeps the accent fill
(that is what Windows does) but the chip no longer dominates the panel
at its reduced height. The "Control center" title is removed.

The volume row keeps its mute glyph, track and percentage, restyled to
the shared slider vocabulary — a thinner track and smaller thumb than
today's.

The bottom of the panel becomes a single bar: battery status on the
left, a settings gear on the right, replacing the two stacked text
links. "Sound output" moves into the body as a `list_row`, where it
belongs with the other navigations.

**Detail pages** share one template: `panel_header` (back button plus
title), then `list_row`s, and nothing else. The descriptive paragraphs
are deleted outright — the row labels already say what they do.

**The Wi-Fi list** gets per-network: a signal-strength glyph chosen from
the percentage (four buckets), a lock mark for secured networks, the
SSID, and a single-line trailing action ("Connect" / "Disconnect"),
right-aligned and never wrapped. The instructional sentence is deleted
and replaced by a scroll affordance: a thin track on the right edge
drawn only when the list overflows.

**Focus** is drawn only when the keyboard put it there. `change_page`
stops seeding `focus = Some(0)`; focus is set by Tab and the arrow keys,
and a `focus_visible` flag gates the ring, exactly as the settings
window now does.

## 5. Calendar

**Scale everything.** Every constant becomes logical and goes through
`runtime::scaled(v, dpi)`; the window and its Direct2D surface are
created at the scaled size; `apply_reveal` scales its height. The
"deliberately unscaled" comment is deleted along with the behavior it
describes.

**The month grid** gets a header row carrying the month and year with
previous/next chevrons, and today becomes a filled accent pill with
contrasting text (`color::accent_text()`), not a colored numeral.
Out-of-month days render in the muted token. Weekday initials use the
caption size.

**The notifications section** gets a proper header row — "Notifications"
at caption weight — and keeps its honest empty state. The placeholder
stays a placeholder: reading the real feed needs `UserNotificationListener`
and a packaged identity this process does not have, which
`apps/ui/src/main.rs` already documents.

## 6. Shared behavior

Both surfaces gain, through the shared widgets:

- **Hover, press and keyboard-focus states** that are distinct from each
  other, with the ring reserved for keyboard focus.
- **Disabled states** drawn in the muted token rather than simply being
  unresponsive (the Wi-Fi chip when no adapter exists, for example).
- **Light, dark and high contrast** from the same tokens the rest of the
  shell resolves against — no literals in either file.

## 7. Testing

The split is the same one that worked for the settings rows: geometry is
pure and tested; drawing is verified by looking.

- `qs_layout` and `targets` already return rects: property tests that no
  two targets overlap, that every target is inside the card, and that
  both hold at 96, 120, 144 and 192 DPI.
- `hit_target` agrees with `targets` for every page.
- Calendar: the month grid's cell rects do not overlap, every cell sits
  inside the grid, the first of the month lands in the correct weekday
  column across a year of months, and the layout scales linearly with
  DPI.
- Signal-strength bucketing is a pure function with a test per boundary.
- Visual verification: capture all six Quick Settings pages and the
  calendar, at 100% and 200%, and compare against this document's
  §2 complaints.

## 8. Out of scope

- The Activities overview.
- Chip reordering or an edit mode: Windows has one, this shell has no
  config to persist the order, and inventing one is a separate change.
- Real notification content (§5).
- Any change to what a control does.
