//! The connections rail: a server→domain tree shown as a left sidebar when more than
//! one directory is connected. Each domain leaf carries a mode badge (`● write` /
//! `○ read-only` / `✎ dry-run`), a `▣` when marked for set operations, and a `◀` on
//! the focused domain whose data fills the workspace. Drawn with the same
//! `mullion::render_tree_row` primitive as the DIT browser.

use mullion::{
    label::Align, render_shared, render_tree_row, table::ColumnGrid,
    Buffer, LineWeight, Node, Rect,
};

use crate::config::ConnMode;
use crate::tui::app::{App, RailRow};
use crate::tui::draw::{hline, vscroll};
use crate::tui::theme::*;

const RAIL: u64 = 1;

pub fn render(app: &App, buf: &mut Buffer, area: Rect) {
    if area.width < 8 || area.height < 4 { return; }
    let has_focus = app.rail_has_focus();

    let mut tree = Node::Tile(RAIL);
    let weight = if has_focus { LineWeight::Heavy } else { LineWeight::Light };
    let rects = render_shared(buf, &mut tree, area, &box_style(), &[(RAIL, weight)]);
    let r = rects.into_iter().find(|(id, _)| *id == RAIL).map(|(_, r)| r).unwrap_or(area);

    let hs = if has_focus { s_head() } else { s_subhead() };
    // Compact so it fits the narrow rail; `⇊N` = N background backups, `⇉N` = N migrations.
    let mut header = format!("connections ({})", app.session_count());
    if app.backups_active() > 0 {
        header.push_str(&format!(" ⇊{}", app.backups_active()));
    }
    if app.migrations_active() > 0 {
        header.push_str(&format!(" ⇉{}", app.migrations_active()));
    }
    if app.comparisons_active() > 0 {
        header.push_str(&format!(" ⇌{}", app.comparisons_active()));
    }
    ColumnGrid::write_text(buf, r, r.y, &header, Align::Start, hs);
    hline(buf, Rect::new(r.x, r.y + 1, r.width, 1));

    let data = Rect::new(r.x, r.y + 2, r.width, r.height.saturating_sub(2));
    let vis = data.height as usize;
    let rows = app.rail_rows();
    let cur = app.rail_cur();
    let content = vscroll(buf, data, cur.offset, rows.len(), vis);
    let theme = mullion_theme();

    for (i, row) in rows.iter().enumerate().skip(cur.offset).take(vis) {
        let y = content.y + (i - cur.offset) as u16;
        let sel = i == cur.cursor && has_focus;
        let label = rail_label(app, row);
        render_tree_row(
            buf, Rect::new(content.x, y, content.width, 1),
            &row.ancestor_last, row.is_last, row.expanded, &label, sel, &theme, dctx(),
        );
    }
}

/// The tree label for a rail row: a bare server name, or a domain with its mode badge
/// plus marked/focused markers baked in (`render_tree_row` styles the row uniformly,
/// so markers travel in the text).
fn rail_label(app: &App, row: &RailRow) -> String {
    if row.is_server {
        return row.label.clone();
    }
    let Some(idx) = row.session_idx else { return row.label.clone() };
    let badge = match app.session_mode(idx) {
        ConnMode::Write => "●",
        ConnMode::ReadOnly => "○",
        ConnMode::DryRun => "✎",
    };
    let mark = if app.is_marked(idx) { "▣ " } else { "" };
    let focus = if idx == app.focused_idx() { " ◀" } else { "" };
    format!("{mark}{} {badge}{focus}", row.label)
}
