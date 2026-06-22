//! Low-level buffer drawing primitives shared across screens and overlays.

use mullion::{style::Style, Buffer, Rect};

use super::theme::s_border;

/// Write a string at `(x, y)` in the given style.
pub fn btxt(buf: &mut Buffer, x: u16, y: u16, text: &str, style: Style) {
    buf.set_string(x, y, text, style);
}

/// Shrink a rect by `n` cells on every side (saturating).
pub fn inset(r: Rect, n: u16) -> Rect {
    Rect::new(r.x + n, r.y + n, r.width.saturating_sub(2 * n), r.height.saturating_sub(2 * n))
}

/// Draw a horizontal rule spanning `r`'s width at `r.y`.
pub fn hline(buf: &mut Buffer, r: Rect) {
    for x in r.x..r.x + r.width {
        buf.set_string(x, r.y, "─", s_border());
    }
}

/// Fill `w` cells starting at `(x, y)` with spaces in `style` (row highlight).
pub fn fill_row(buf: &mut Buffer, x: u16, y: u16, w: u16, style: Style) {
    for cx in x..x + w {
        buf.set_string(cx, y, " ", style);
    }
}
