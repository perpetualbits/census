//! Browse screen: user list (left) beside a per-user detail pane (right).

use mullion::{
    label::Align,
    render_shared,
    table::{ColumnDef, ColumnGrid, ColumnKind},
    Buffer, Constraint, LineWeight, Node, Orientation, Rect, Size,
};

use crate::tui::app::App;
use crate::tui::draw::{btxt, fill_row, hline, keyhints, vscroll};
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
            let count = format!(" {} users ", app.users().len());
            let cw = count.chars().count() as u16;
            let hint_w = area.width.saturating_sub(4).saturating_sub(cw);
            keyhints(buf, area.x + 2, bottom, hint_w, pairs);
            let cx = area.x + area.width.saturating_sub(1 + cw);
            btxt(buf, cx, bottom, &count, s_dim());
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
    let hs    = if focused { s_head() } else { s_subhead() };
    let label = if app.users_truncated() {
        format!("users ({} — capped)", app.users().len())
    } else {
        format!("users ({})", app.users().len())
    };
    ColumnGrid::write_text(buf, area, area.y, &label, Align::Start, hs);
    hline(buf, Rect::new(area.x, area.y + 1, area.width, 1));

    let data = Rect::new(area.x, area.y + 2, area.width, area.height.saturating_sub(2));
    let vis  = data.height as usize;
    let cur  = &app.users_cur;
    let content = vscroll(buf, data, cur.offset, app.users().len(), vis);
    let cols = list_grid().resolve(content);

    for (i, user) in app.users().iter().enumerate().skip(cur.offset).take(vis) {
        let y   = content.y + (i - cur.offset) as u16;
        let sel = i == cur.cursor;
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
