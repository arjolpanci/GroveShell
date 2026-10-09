//! Settings rows: the thing a page is made of.
//!
//! A page declares a list of [`Card`]s, each holding [`Row`]s. One layout
//! pass ([`layout::layout_page`]) turns that into rectangles, and paint,
//! hit-testing and keyboard traversal all read those same rectangles.
//! Nothing downstream computes a rect of its own, so what is drawn, what
//! is clickable, and what Tab reaches cannot drift apart — the rule the
//! shell already follows for its bar regions and Quick Settings controls.

pub mod layout;

/// The control on the right-hand side of a row.
#[derive(Clone, Debug, PartialEq)]
pub enum Control {
    /// A switch, with its own `On`/`Off` label drawn to its left — the
    /// way Windows 11 Settings labels a toggle.
    Toggle { on: bool },
    /// A track with a thumb and a live value label.
    Slider { value: f32, min: f32, max: f32, unit: &'static str },
    /// A dropdown showing the selected option.
    Choice { options: &'static [&'static str], selected: usize },
    /// A push button.
    Action { label: &'static str },
    /// The row's title is itself the link; the whole row is clickable.
    Link,
    /// A state report: a severity glyph plus the row's two lines.
    Status { severity: Severity },
    /// No control — a row that is only text.
    None,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Ok,
    Warning,
    Error,
}

/// One setting. `id` is the page's own stable identifier for it: clicks,
/// value changes and focus all travel by id, never by index, so inserting
/// a row above another cannot silently rewire what a click does.
#[derive(Clone, Debug)]
pub struct Row {
    pub id: u32,
    pub glyph: Option<&'static str>,
    pub title: String,
    pub description: Option<String>,
    pub control: Control,
    pub enabled: bool,
}

impl Row {
    pub fn new(id: u32, title: impl Into<String>) -> Self {
        Self {
            id,
            glyph: None,
            title: title.into(),
            description: None,
            control: Control::None,
            enabled: true,
        }
    }

    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    pub fn with_glyph(mut self, glyph: &'static str) -> Self {
        self.glyph = Some(glyph);
        self
    }

    pub fn with_control(mut self, control: Control) -> Self {
        self.control = control;
        self
    }

    pub fn disabled(mut self) -> Self {
        self.enabled = false;
        self
    }
}

/// A group of rows drawn on one rounded surface, under an optional
/// caption.
#[derive(Clone, Debug)]
pub struct Card {
    pub caption: Option<String>,
    pub rows: Vec<Row>,
}

impl Card {
    pub fn new(rows: Vec<Row>) -> Self {
        Self { caption: None, rows }
    }

    pub fn with_caption(caption: impl Into<String>, rows: Vec<Row>) -> Self {
        Self { caption: Some(caption.into()), rows }
    }
}
