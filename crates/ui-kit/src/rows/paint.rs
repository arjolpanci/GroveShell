//! Drawing a laid-out page through [`Canvas`].
//!
//! Reads the rectangles [`super::layout::layout_page`] produced and never
//! computes one of its own, so what is drawn is exactly what
//! [`super::input::hit_test`] will match a click against.
//!
//! Every color comes from `design::color`, which follows the system
//! light/dark theme, the live Windows accent and high contrast. There are
//! no literals here on purpose: the previous settings window hardcoded
//! its palette and ended up matching neither Windows nor the shell.

use windows::Win32::Foundation::{COLORREF, RECT};
use windows::Win32::Graphics::Gdi::{DT_END_ELLIPSIS, DT_LEFT, DT_RIGHT, DT_SINGLELINE, DT_VCENTER};

use super::layout::{PageLayout, RowRects, ROW_PAD_X};
use super::{Card, Control, Row, Severity};
use crate::canvas::Canvas;
use crate::design::{color, metrics, typography};
use crate::glyph;
use crate::runtime::scaled;

/// Corner radius of a card, measured off Windows 11 Settings.
const CARD_RADIUS: i32 = 4;
/// Height of a toggle's pill.
const TOGGLE_HEIGHT: i32 = 20;
const TOGGLE_WIDTH: i32 = 40;
const SLIDER_TRACK_HEIGHT: i32 = 4;
const SLIDER_THUMB_RADIUS: i32 = 8;

/// Paints every card and row. `focused` and `hovered` are row ids;
/// `scroll` shifts the whole page up by that many pixels.
pub fn paint_page(
    canvas: &mut dyn Canvas,
    cards: &[Card],
    layout: &PageLayout,
    focused: Option<u32>,
    hovered: Option<u32>,
    scroll: i32,
    dpi: u32,
) {
    for (card_index, (card_rect, row_rects)) in layout.cards.iter().enumerate() {
        let Some(card) = cards.get(card_index) else { continue };

        if let Some(caption) = &card.caption {
            let height = scaled(28, dpi);
            let rect = shift(
                RECT { left: card_rect.left, top: card_rect.top - height, right: card_rect.right, bottom: card_rect.top },
                scroll,
            );
            canvas.set_text_color(COLORREF(color::text()));
            canvas.set_font_size(typography::CAPTION_PX);
            canvas.text(rect, caption, DT_LEFT | DT_SINGLELINE | DT_VCENTER);
        }

        canvas.fill_round_rect(
            shift(*card_rect, scroll),
            scaled(CARD_RADIUS, dpi),
            COLORREF(color::surface_raised()),
        );

        for (row_index, rects) in row_rects.iter().enumerate() {
            let Some(row) = card.rows.get(row_index) else { continue };
            paint_row(canvas, row, rects, focused == Some(row.id), hovered == Some(row.id), scroll, dpi);

            // A hairline between adjacent rows, never after the last one:
            // the card's own rounded edge ends the group.
            if row_index + 1 < row_rects.len() {
                let r = shift(rects.row, scroll);
                canvas.fill_rect(
                    RECT { left: r.left, top: r.bottom - 1, right: r.right, bottom: r.bottom },
                    COLORREF(color::stroke()),
                );
            }
        }
    }
}

fn shift(rect: RECT, scroll: i32) -> RECT {
    RECT { left: rect.left, top: rect.top - scroll, right: rect.right, bottom: rect.bottom - scroll }
}

fn paint_row(
    canvas: &mut dyn Canvas,
    row: &Row,
    rects: &RowRects,
    focused: bool,
    hovered: bool,
    scroll: i32,
    dpi: u32,
) {
    let r = shift(rects.row, scroll);
    if hovered && row.enabled {
        canvas.fill_rect(r, COLORREF(color::surface_overlay()));
    }

    // A disabled row draws every part muted, control included, so it
    // reads as unavailable rather than merely unresponsive.
    let text_color = if row.enabled { color::text() } else { color::text_muted() };

    if let Some(g) = row.glyph {
        canvas.set_text_color(COLORREF(text_color));
        canvas.glyph(shift(rects.glyph, scroll), g);
    }

    let text = shift(rects.text, scroll);
    match &row.description {
        Some(description) => {
            let half = (text.bottom - text.top) / 2;
            let title_rect = RECT { bottom: text.top + half, ..text };
            let description_rect = RECT { top: text.top + half, ..text };

            canvas.set_text_color(COLORREF(if matches!(row.control, Control::Link) {
                color::accent()
            } else {
                text_color
            }));
            canvas.set_font_size(typography::BODY_PX);
            canvas.text(title_rect, &row.title, DT_LEFT | DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS);

            canvas.set_text_color(COLORREF(color::text_muted()));
            canvas.set_font_size(typography::CAPTION_PX);
            canvas.text(
                description_rect,
                description,
                DT_LEFT | DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS,
            );
        }
        None => {
            canvas.set_text_color(COLORREF(if matches!(row.control, Control::Link) {
                color::accent()
            } else {
                text_color
            }));
            canvas.set_font_size(typography::BODY_PX);
            canvas.text(text, &row.title, DT_LEFT | DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS);
        }
    }

    paint_control(canvas, row, shift(rects.control, scroll), r, dpi);

    if focused {
        // The system focus rectangle: a stroke just inside the row, in
        // the text color so it survives high contrast.
        canvas.stroke_round_rect(r, scaled(CARD_RADIUS, dpi), COLORREF(color::text()), 2.0);
    }
}

/// `rect` is the control itself; `row_rect` is the whole row, used by
/// the slider to place its value label beyond the track (see
/// `layout::SLIDER_LABEL_WIDTH` for why the label is not inside `rect`).
fn paint_control(canvas: &mut dyn Canvas, row: &Row, rect: RECT, row_rect: RECT, dpi: u32) {
    let enabled = row.enabled;
    match &row.control {
        Control::Toggle { on } => {
            let width = scaled(TOGGLE_WIDTH, dpi);
            let height = scaled(TOGGLE_HEIGHT, dpi);
            let pill = RECT {
                left: rect.right - width,
                top: (rect.top + rect.bottom) / 2 - height / 2,
                right: rect.right,
                bottom: (rect.top + rect.bottom) / 2 + height / 2,
            };

            // Windows labels the state in words beside the switch; a
            // colored pill alone is not readable in high contrast.
            canvas.set_text_color(COLORREF(color::text_muted()));
            canvas.set_font_size(typography::CAPTION_PX);
            canvas.text(
                RECT { right: pill.left - scaled(8, dpi), ..rect },
                if *on { "On" } else { "Off" },
                DT_RIGHT | DT_SINGLELINE | DT_VCENTER,
            );

            let track = if *on && enabled { color::accent() } else { color::stroke() };
            canvas.fill_round_rect(pill, height / 2, COLORREF(track));
            if !on || !enabled {
                canvas.stroke_round_rect(pill, height / 2, COLORREF(color::text_muted()), 1.0);
            }

            let inset = scaled(3, dpi);
            let diameter = height - inset * 2;
            let thumb_left = if *on { pill.right - inset - diameter } else { pill.left + inset };
            canvas.fill_ellipse(
                RECT {
                    left: thumb_left,
                    top: pill.top + inset,
                    right: thumb_left + diameter,
                    bottom: pill.bottom - inset,
                },
                COLORREF(if *on && enabled { color::accent_text() } else { color::text() }),
            );
        }
        Control::Slider { value, min, max, unit } => {
            // The track spans the whole control rect: that rect is what
            // `input::hit_test` maps a click across, so a track drawn
            // any narrower would put the thumb somewhere the cursor is
            // not.
            let track_height = scaled(SLIDER_TRACK_HEIGHT, dpi);
            let middle = (rect.top + rect.bottom) / 2;
            let track = RECT {
                left: rect.left,
                top: middle - track_height / 2,
                right: rect.right,
                bottom: middle + track_height / 2,
            };
            canvas.fill_round_rect(track, track_height / 2, COLORREF(color::stroke()));

            let span = (max - min).abs().max(f32::EPSILON);
            let fraction = ((value - min) / span).clamp(0.0, 1.0);
            let filled_right = track.left + ((track.right - track.left) as f32 * fraction).round() as i32;
            if filled_right > track.left {
                canvas.fill_round_rect(
                    RECT { right: filled_right, ..track },
                    track_height / 2,
                    COLORREF(if enabled { color::accent() } else { color::text_muted() }),
                );
            }

            let radius = scaled(SLIDER_THUMB_RADIUS, dpi);
            canvas.fill_ellipse(
                RECT {
                    left: filled_right - radius,
                    top: middle - radius,
                    right: filled_right + radius,
                    bottom: middle + radius,
                },
                COLORREF(if enabled { color::accent() } else { color::text_muted() }),
            );

            canvas.set_text_color(COLORREF(color::text()));
            canvas.set_font_size(typography::BODY_PX);
            canvas.text(
                RECT { left: track.right, right: row_rect.right - scaled(ROW_PAD_X, dpi), ..rect },
                &format!("{}{}", value.round() as i32, unit),
                DT_RIGHT | DT_SINGLELINE | DT_VCENTER,
            );
        }
        Control::Choice { options, selected } => {
            canvas.fill_round_rect(
                inset_vertically(rect, dpi),
                scaled(CARD_RADIUS, dpi),
                COLORREF(color::surface_overlay()),
            );
            canvas.stroke_round_rect(
                inset_vertically(rect, dpi),
                scaled(CARD_RADIUS, dpi),
                COLORREF(color::stroke()),
                1.0,
            );
            canvas.set_text_color(COLORREF(if enabled { color::text() } else { color::text_muted() }));
            canvas.set_font_size(typography::BODY_PX);
            let chevron = scaled(24, dpi);
            canvas.text(
                RECT { left: rect.left + scaled(12, dpi), right: rect.right - chevron, ..rect },
                options.get(*selected).copied().unwrap_or(""),
                DT_LEFT | DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS,
            );
            canvas.glyph(
                RECT { left: rect.right - chevron, ..rect },
                glyph::CHEVRON_DOWN,
            );
        }
        Control::Action { label } => {
            let button = inset_vertically(rect, dpi);
            canvas.fill_round_rect(button, scaled(CARD_RADIUS, dpi), COLORREF(color::surface_overlay()));
            canvas.stroke_round_rect(button, scaled(CARD_RADIUS, dpi), COLORREF(color::stroke()), 1.0);
            canvas.set_text_color(COLORREF(if enabled { color::text() } else { color::text_muted() }));
            canvas.set_font_size(typography::BODY_PX);
            canvas.text(button, label, DT_SINGLELINE | DT_VCENTER | windows::Win32::Graphics::Gdi::DT_CENTER);
        }
        Control::Status { severity } => {
            let size = scaled(20, dpi);
            let middle = (rect.top + rect.bottom) / 2;
            canvas.set_text_color(COLORREF(match severity {
                Severity::Ok => color::accent(),
                Severity::Warning | Severity::Error => color::text(),
            }));
            canvas.glyph(
                RECT {
                    left: rect.right - size,
                    top: middle - size / 2,
                    right: rect.right,
                    bottom: middle + size / 2,
                },
                match severity {
                    Severity::Ok => glyph::OK,
                    Severity::Warning => glyph::WARNING,
                    Severity::Error => glyph::ERROR,
                },
            );
        }
        Control::Link | Control::None => {}
    }
}

/// A control box inset from the row's full height, so buttons and
/// dropdowns do not touch the dividers above and below them.
fn inset_vertically(rect: RECT, dpi: u32) -> RECT {
    let inset = scaled(metrics::SPACING, dpi);
    RECT { top: rect.top + inset, bottom: rect.bottom - inset, ..rect }
}
