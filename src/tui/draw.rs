//! Low-level buffer drawing primitives shared across screens and overlays.

use mullion::{render_keyhints, render_scrollbar, style::Style, Buffer, Rect, ScrollMetrics, TextCtx};

use super::theme::{mullion_theme, s_border, s_dim};

/// Write a string at `(x, y)` in the given style.
pub fn btxt(buf: &mut Buffer, x: u16, y: u16, text: &str, style: Style) {
    buf.set_string(x, y, text, style);
}

/// Draw a horizontal rule spanning `r`'s width at `r.y`.
pub fn hline(buf: &mut Buffer, r: Rect) {
    for x in r.x..r.x + r.width {
        buf.set_string(x, r.y, "─", s_border());
    }
}

/// Draw a themed key-hint footer bar (`key label · …`) into row `y`, spanning
/// `w` columns from `x`, via mullion's `render_keyhints` in census colours.
pub fn keyhints(buf: &mut Buffer, x: u16, y: u16, w: u16, pairs: &[(&str, &str)]) {
    render_keyhints(buf, Rect::new(x, y, w, 1), pairs, &mullion_theme(), TextCtx::LTR);
}

/// Fill `w` cells starting at `(x, y)` with spaces in `style` (row highlight).
pub fn fill_row(buf: &mut Buffer, x: u16, y: u16, w: u16, style: Style) {
    for cx in x..x + w {
        buf.set_string(cx, y, " ", style);
    }
}

/// Draw a vertical scrollbar in the rightmost column of `area` when a list of `len`
/// rows overflows a `vis`-row viewport scrolled to `offset`, and return the content
/// rect (`area` minus the scrollbar column). When the list fits, `area` is returned
/// unchanged and no bar is drawn. Uses mullion's [`render_scrollbar`]; the metrics
/// follow the engine convention (`position` = fraction of rows above the top).
pub fn vscroll(buf: &mut Buffer, area: Rect, offset: usize, len: usize, vis: usize) -> Rect {
    if len <= vis || vis == 0 || area.width < 2 {
        return area;
    }
    let bar = Rect::new(area.x + area.width - 1, area.y, 1, area.height);
    render_scrollbar(buf, bar, ScrollMetrics::from_window(offset, vis, len), s_dim());
    Rect::new(area.x, area.y, area.width - 1, area.height)
}
