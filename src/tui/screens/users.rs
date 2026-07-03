//! Browse screen: user list (left) beside a per-user detail pane (right).

use mullion::{
    label::Align,
    render_scrollbar, render_shared,
    table::{ColumnDef, ColumnGrid, ColumnKind},
    Buffer, Constraint, LineWeight, Node, Orientation, Rect, Size,
};

use crate::tui::app::App;
use crate::tui::draw::{btxt, fill_row, hline, keyhints};
use crate::tui::focus::Pane;
use crate::tui::theme::*;

use super::detail;

/// Stable tile ids for the browse layout.
const LIST: u64 = 1;
const DETAIL: u64 = 2;

pub fn render(app: &App, buf: &mut Buffer, area: Rect, focus: Pane) {
    if area.width < 20 || area.height < 5 { return; }

    // List pane (left) beside the detail pane (right). The engine draws the
    // outer frame and the shared divider (with junctions) for us; the focused
    // pane's border is thickened to a heavy weight.
    let mut tree = Node::Split {
        orientation: Orientation::Horizontal,
        children: vec![
            (Constraint::new(Size::Percent(40)).with_min(20).with_max(44), Node::Tile(LIST)),
            (Constraint::new(Size::Fill(1)), Node::Tile(DETAIL)),
        ],
    };
    let focused = if focus == Pane::Left { LIST } else { DETAIL };
    let rects = render_shared(
        buf, &mut tree, area, &box_style(),
        &[(focused, LineWeight::Heavy)],
    );

    btxt(buf, area.x + 2, area.y, &format!("  census · {}  ", app.mode_tag()), s_title());

    let bottom = area.y + area.height - 1;
    match &app.status {
        Some((msg, is_err)) => {
            btxt(buf, area.x + 2, bottom, &format!(" {msg} "),
                 if *is_err { s_err() } else { s_ok() });
        }
        None => {
            let pairs: &[(&str, &str)] = if focus == Pane::Right {
                &[("Tab", "pane"), ("jk", "attr"), ("e", "edit"), ("E", "big-edit"), ("K", "keys"),
                  ("p", "passwd"), ("u", "undo"), ("L", "ldif"), ("?", "help"), ("q", "quit")]
            } else {
                &[("Tab", "pane"), ("jk", "users"), ("/", "search"), ("n", "new"), ("D", "del"),
                  ("g", "groups"), ("t", "tree"), ("u", "undo"), ("?", "help"), ("q", "quit")]
            };
            keyhints(buf, area.x + 2, bottom, area.width.saturating_sub(4), pairs);
        }
    }

    for (id, r) in rects {
        match id {
            LIST   => render_list(app, buf, r, focus == Pane::Left),
            DETAIL => detail::render(app, buf, r, focus == Pane::Right),
            _ => {}
        }
    }
}

fn render_list(app: &App, buf: &mut Buffer, area: Rect, focused: bool) {
    let hs   = if focused { s_head() } else { s_subhead() };
    let list = app.user_list();
    // The list is windowed over the directory — the total is unknown, so show the
    // window ("N shown", `+` when more exist below), not a (misleading) count.
    let more = if list.at_top() && list.at_bottom() { "" } else { "+" };
    let mut label = format!("users ({}{more} shown)", list.visible().len());
    // On a server without Server-Side Sort the browse is a capped, client-sorted
    // window — say so, since you can't page the whole (millions-row) set in order.
    if !app.browse_keyset() { label.push_str(" · capped (no server sort)"); }
    if app.browse_err() { label.push_str(" — browse error"); }
    ColumnGrid::write_text(buf, area, area.y, &label, Align::Start, hs);
    hline(buf, Rect::new(area.x, area.y + 1, area.width, 1));

    let data = Rect::new(area.x, area.y + 2, area.width, area.height.saturating_sub(2));
    // Estimated scrollbar in the rightmost column unless the whole set is on screen.
    let content = if !(list.at_top() && list.at_bottom()) && data.width >= 2 {
        let bar = Rect::new(data.x + data.width - 1, data.y, 1, data.height);
        render_scrollbar(buf, bar, app.user_metrics(), s_dim());
        Rect::new(data.x, data.y, data.width - 1, data.height)
    } else {
        data
    };
    let cols = list_grid().resolve(content);

    for (row, user) in list.visible().iter().enumerate() {
        let y   = content.y + row as u16;
        let sel = list.selected_visible_row() == Some(row);
        let sty = if sel { s_sel() } else if focused { s_normal() } else { s_dim() };
        if sel { fill_row(buf, content.x, y, content.width, sty); }
        ColumnGrid::write_text(buf, cols[0], y, &user.uid, Align::Start, sty);
        ColumnGrid::write_text_ctx(buf, cols[2], y, &user.cn, Align::Start, sty, dctx());
    }
}

fn list_grid() -> ColumnGrid {
    ColumnGrid::new(vec![
        ColumnDef::fixed(12, ColumnKind::Text),             // uid
        ColumnDef::fixed(1,  ColumnKind::Custom),           // gap
        ColumnDef::fill(1,   ColumnKind::Text).with_min(6), // name
    ])
}
