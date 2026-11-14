//! Layout and CSS-class decorations for Aura containers and widgets.
//!
//! The core Aura grammar has one `Container` kind. The GTK backend lets the
//! host choose, per mounted widget, how a container lays out its children
//! (vertical or horizontal stack, or a grid that fills row by row) and which
//! CSS classes a widget carries. [`crate::GtkHost`] keeps these by Aura key
//! and re-applies them after every commit, so they follow a key across
//! reorder and reparent and are applied afresh when a kind change replaces
//! the widget. These helpers are pure and always compiled.

use crate::GtkError;

/// Largest spacing, grid column count and class count accepted.
pub const MAX_SPACING: u16 = 1024;
pub const MAX_GRID_COLUMNS: u16 = 256;
pub const MAX_CLASSES: usize = 16;
pub const MAX_CLASS_LEN: usize = 64;

/// How a container arranges its children.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Layout {
    /// `GtkBoxLayout`, vertical (the default for every container).
    VStack { spacing: u16 },
    /// `GtkBoxLayout`, horizontal.
    HStack { spacing: u16 },
    /// `GtkGridLayout`; child `i` goes to column `i % columns`, row
    /// `i / columns`, recomputed after every transaction.
    Grid {
        columns: u16,
        row_spacing: u16,
        column_spacing: u16,
    },
}

impl Default for Layout {
    fn default() -> Self {
        Layout::VStack { spacing: 0 }
    }
}

impl Layout {
    pub fn validate(self) -> Result<Self, GtkError> {
        let spacing_ok = |s: u16| s <= MAX_SPACING;
        match self {
            Layout::VStack { spacing } | Layout::HStack { spacing } if !spacing_ok(spacing) => {
                Err(GtkError::Unsupported("layout spacing too large"))
            }
            Layout::Grid { columns, .. } if columns == 0 || columns > MAX_GRID_COLUMNS => {
                Err(GtkError::Unsupported("grid columns out of range"))
            }
            Layout::Grid {
                row_spacing,
                column_spacing,
                ..
            } if !spacing_ok(row_spacing) || !spacing_ok(column_spacing) => {
                Err(GtkError::Unsupported("layout spacing too large"))
            }
            _ => Ok(self),
        }
    }
}

/// Grid cell `(column, row)` of child `index`. `columns` must be nonzero.
pub fn grid_cell(index: usize, columns: u16) -> Result<(i32, i32), GtkError> {
    let columns = usize::from(columns);
    if columns == 0 {
        return Err(GtkError::Unsupported("grid columns out of range"));
    }
    let col = i32::try_from(index % columns).map_err(|_| GtkError::Unsupported("grid index"))?;
    let row = i32::try_from(index / columns).map_err(|_| GtkError::Unsupported("grid index"))?;
    Ok((col, row))
}

/// A CSS class name: an ASCII identifier (`[A-Za-z_][A-Za-z0-9_-]*`), at most
/// [`MAX_CLASS_LEN`] bytes. GTK would accept more, but a class that a
/// stylesheet selector cannot name is a host bug worth rejecting.
pub fn validate_css_class(class: &str) -> Result<(), GtkError> {
    let mut bytes = class.bytes();
    let first_ok = matches!(bytes.next(), Some(b'a'..=b'z' | b'A'..=b'Z' | b'_'));
    if !first_ok
        || class.len() > MAX_CLASS_LEN
        || !bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    {
        return Err(GtkError::Unsupported("invalid CSS class name"));
    }
    Ok(())
}

/// Host-chosen decorations of one keyed widget.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Style {
    /// Only meaningful on containers; `None` means the default vertical stack.
    pub layout: Option<Layout>,
    /// Sorted and deduplicated on validation.
    pub classes: Vec<String>,
}

impl Style {
    pub fn layout(mut self, layout: Layout) -> Self {
        self.layout = Some(layout);
        self
    }
    pub fn class(mut self, class: impl Into<String>) -> Self {
        self.classes.push(class.into());
        self
    }
    /// Validate and normalize (sorted, deduplicated classes).
    pub fn validate(mut self) -> Result<Self, GtkError> {
        if let Some(layout) = self.layout {
            layout.validate()?;
        }
        for class in &self.classes {
            validate_css_class(class)?;
        }
        self.classes.sort();
        self.classes.dedup();
        if self.classes.len() > MAX_CLASSES {
            return Err(GtkError::Unsupported("too many CSS classes"));
        }
        Ok(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layouts_and_classes_are_validated() {
        assert!(Layout::VStack { spacing: 4 }.validate().is_ok());
        assert!(Layout::HStack { spacing: 2000 }.validate().is_err());
        let grid = |columns| Layout::Grid {
            columns,
            row_spacing: 1,
            column_spacing: 1,
        };
        assert!(grid(0).validate().is_err());
        assert!(grid(3).validate().is_ok());
        assert!(grid(MAX_GRID_COLUMNS + 1).validate().is_err());
        assert_eq!(grid_cell(7, 3), Ok((1, 2)));
        assert!(grid_cell(1, 0).is_err());
        for ok in ["card", "_x", "a-b_9"] {
            assert_eq!(validate_css_class(ok), Ok(()), "{ok}");
        }
        for bad in ["", "9a", "-a", "a b", "a.b", "a{", "é"] {
            assert!(validate_css_class(bad).is_err(), "{bad:?}");
        }
        assert!(validate_css_class(&"a".repeat(MAX_CLASS_LEN + 1)).is_err());
        let style = Style::default()
            .class("b")
            .class("a")
            .class("b")
            .validate()
            .unwrap();
        assert_eq!(style.classes, ["a", "b"]);
        let many = (0..=MAX_CLASSES).fold(Style::default(), |s, i| s.class(format!("c{i}")));
        assert!(many.validate().is_err());
    }
}
