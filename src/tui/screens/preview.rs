//! The toggleable LDIF change-preview tile (bottom strip).
//!
//! Renders the session's change feed in LDIF: every write census has made (or, in
//! `--dry-run`, would make) plus the on-disk journal path. Shares the LDIF
//! serializer with the rollback journal, so the preview is exactly what gets
//! written/replayed.

use mullion::{render_shared, Buffer, Node, Rect};

use crate::tui::app::App;
use crate::tui::draw::{btxt, vscroll};
use crate::tui::theme::*;

const PREVIEW: u64 = 1;

pub fn render(app: &App, buf: &mut Buffer, area: Rect) {
    if area.width < 12 || area.height < 3 {
        return;
    }
    let mut tree = Node::Tile(PREVIEW);
    let rects = render_shared(buf, &mut tree, area, &box_style(), &[]);
    let inner = rects[0].1;

    let title = format!("  changes — LDIF ({} undoable)  ", app.undo_depth());
    btxt(buf, area.x + 2, area.y, &title, s_title());

    // Footer: where the on-disk journal lives.
    let foot = format!(" journal: {} ", app.journal_path().display());
    btxt(buf, area.x + 2, area.y + area.height - 1, &clip(&foot, area.width.saturating_sub(4)), s_dim());

    // Flatten the change feed into individual LDIF lines, newest last.
    let lines: Vec<&str> = app.journal_log().iter().flat_map(|b| b.lines()).collect();
    if lines.is_empty() {
        btxt(buf, inner.x, inner.y, "(no changes yet)", s_dim());
        return;
    }

    let vis = inner.height as usize;
    let start = lines.len().saturating_sub(vis); // tail: keep the most recent in view
    let content = vscroll(buf, inner, start, lines.len(), vis);

    for (i, line) in lines.iter().enumerate().skip(start).take(vis) {
        let y = content.y + (i - start) as u16;
        let sty = if line.starts_with('#') {
            s_dim()
        } else if line.starts_with("dn:") {
            s_subhead()
        } else if line.starts_with("changetype:") || *line == "-" {
            s_title()
        } else {
            s_normal()
        };
        btxt(buf, content.x, y, &clip(line, content.width), sty);
    }
}

/// Truncate `s` to at most `w` display columns (char-approximate, ASCII LDIF).
fn clip(s: &str, w: u16) -> String {
    s.chars().take(w as usize).collect()
}
