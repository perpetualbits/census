//! A bookended "gap" in the top border showing how census reached the directory.
//!
//! Drawn after the screen's frame (structure) and before the travelling rim glow
//! (animation), following mullion's [`BorderGap`] three-pass pattern: the returned
//! gap covers the *text* cells so the glow skips them, while the `┤`/`├` bookend
//! caps sit just outside it and so keep catching the glow as it passes.

use mullion::{label::Side, socket::bookends, BorderGap, Buffer, Rect};

use super::draw::btxt;
use super::theme::{s_border, s_subhead};

/// Carve a centred, bookended gap into the top edge of `area` displaying `text`,
/// returning the [`BorderGap`] over its content cells (or `None` when the terminal
/// is too narrow to host the gap with border left on either side).
pub fn draw_top_gap(buf: &mut Buffer, area: Rect, text: &str) -> Option<BorderGap> {
    let content = format!(" {text} ");
    let clen = content.chars().count() as u16;
    let total = clen + 2; // one cap on each side
    // Keep a healthy run of border either side of the gap (and clear of corners).
    if area.width < total + 12 {
        return None;
    }
    let (lcap, rcap) = bookends(Side::Top);
    let x = area.x + (area.width - total) / 2;
    let y = area.y;
    btxt(buf, x, y, lcap, s_border());
    btxt(buf, x + 1, y, &content, s_subhead());
    btxt(buf, x + 1 + clen, y, rcap, s_border());
    Some(BorderGap::new(Rect::new(x + 1, y, clen, 1)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The top row of the buffer, as a string, for assertions.
    fn top_row(buf: &Buffer, area: Rect) -> String {
        (area.x..area.x + area.width).map(|x| buf.get(x, area.y).symbol.clone()).collect()
    }

    #[test]
    fn draws_bookended_gap_and_returns_it() {
        let area = Rect::new(0, 0, 60, 6);
        let mut buf = Buffer::empty(area);
        let gap = draw_top_gap(&mut buf, area, "tunnel ldap.lofar.eu · LDAPS · rbw")
            .expect("gap should fit in a width-60 area");
        let row = top_row(&buf, area);
        assert!(row.contains('┤') && row.contains('├'), "bookend caps present: {row:?}");
        assert!(row.contains("tunnel ldap.lofar.eu"), "content present: {row:?}");
        assert!(row.contains("rbw"), "password source present: {row:?}");
        // The returned gap covers the content cells, not the caps, so the glow
        // skips the text but lights the caps.
        assert_eq!(gap.rect.y, 0);
        assert!(!gap.rim_glow);
        assert_eq!(buf.get(gap.rect.x, 0).symbol, " "); // leading pad inside the gap
    }

    #[test]
    fn narrow_terminal_yields_no_gap() {
        let area = Rect::new(0, 0, 20, 6);
        let mut buf = Buffer::empty(area);
        assert!(draw_top_gap(&mut buf, area, "tunnel ldap.lofar.eu · LDAPS · rbw").is_none());
    }
}
