//! Named Segoe Fluent Icons code points, so a settings row declares
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
pub const CHEVRON_LEFT: &str = "\u{E76B}";
pub const CHEVRON_RIGHT: &str = "\u{E76C}";
pub const OK: &str = "\u{E73E}";
pub const WARNING: &str = "\u{E7BA}";
pub const ERROR: &str = "\u{EA39}";

/// Every constant above, for the test that validates them.
#[cfg(test)]
const ALL: &[(&str, &str)] = &[
    ("RESIZE", RESIZE),
    ("BACKDROP", BACKDROP),
    ("DOCK", DOCK),
    ("OVERVIEW", OVERVIEW),
    ("WORKSPACES", WORKSPACES),
    ("INPUT", INPUT),
    ("ACCESSIBILITY", ACCESSIBILITY),
    ("HOME", HOME),
    ("ABOUT", ABOUT),
    ("STARTUP", STARTUP),
    ("CHEVRON_DOWN", CHEVRON_DOWN),
    ("CHEVRON_LEFT", CHEVRON_LEFT),
    ("CHEVRON_RIGHT", CHEVRON_RIGHT),
    ("OK", OK),
    ("WARNING", WARNING),
    ("ERROR", ERROR),
];

#[cfg(test)]
mod tests {
    /// Every constant must be exactly one character inside the private
    /// use area Segoe Fluent Icons occupies. This catches a typo'd or
    /// truncated escape, which would otherwise render as a blank or a
    /// tofu box and look like a font problem. It cannot check that a code
    /// point is the *right* picture — that is verified by looking at the
    /// nav rail and the settings rows.
    #[test]
    fn every_glyph_is_one_private_use_character() {
        for (name, value) in super::ALL {
            let mut chars = value.chars();
            let c = chars.next().unwrap_or('\0');
            assert!(chars.next().is_none(), "{name} is more than one character");
            assert!(
                ('\u{E700}'..='\u{F8FF}').contains(&c),
                "{name} is U+{:04X}, outside the Segoe Fluent private use area",
                c as u32
            );
        }
    }
}
