//! DIT tree browser: the directory tree (left) beside the selected entry's
//! attributes (right). The tree is drawn with mullion's outline primitive; entry
//! values render bidi-correct via `write_text_ctx`.

use mullion::{
    label::Align,
    render_shared, render_tree_row,
    table::{ColumnDef, ColumnGrid, ColumnKind},
    Buffer, Constraint, LineWeight, Node, Orientation, Rect, Size,
};

use crate::tui::app::App;
use crate::tui::draw::{btxt, fill_row, hline, keyhints, vscroll};
use crate::tui::focus::Pane;
use crate::tui::theme::*;

const TREE:  u64 = 1;
const ENTRY: u64 = 2;

pub fn render(app: &App, buf: &mut Buffer, area: Rect) {
    if area.width < 20 || area.height < 5 { return; }
    let focus = app.dit_focus;

    let mut tree = Node::Split {
        orientation: Orientation::Horizontal,
        children: vec![
            (Constraint::new(Size::Percent(50)).with_min(24), Node::Tile(TREE)),
            (Constraint::new(Size::Fill(1)), Node::Tile(ENTRY)),
        ],
    };
    let focused = if focus == Pane::Left { TREE } else { ENTRY };
    let rects = render_shared(buf, &mut tree, area, &box_style(), &[(focused, LineWeight::Heavy)]);

    btxt(buf, area.x + 2, area.y, &format!("  census — DIT: {}  ", app.dit_base()), s_title());

    let bottom = area.y + area.height - 1;
    match &app.status {
        Some((msg, is_err)) => {
            btxt(buf, area.x + 2, bottom, &format!(" {msg} "),
                 if *is_err { s_err() } else { s_ok() });
        }
        None => {
            let pairs: &[(&str, &str)] = if focus == Pane::Right {
                &[("Tab", "tree"), ("jk", "attr"), ("e", "edit"), ("E", "big-edit"),
                  ("D", "delete"), ("Esc", "back")]
            } else {
                &[("jk", "nav"), ("l/Enter", "expand"), ("h", "collapse"), ("Tab", "entry"),
                  ("D", "del"), ("u", "undo"), ("?", "help"), ("Esc", "back")]
            };
            keyhints(buf, area.x + 2, bottom, area.width.saturating_sub(4), pairs);
        }
    }

    for (id, r) in rects {
        match id {
            TREE  => render_tree(app, buf, r, focus == Pane::Left),
            ENTRY => render_entry(app, buf, r, focus == Pane::Right),
            _ => {}
        }
    }
}

fn render_tree(app: &App, buf: &mut Buffer, area: Rect, focused: bool) {
    if area.width < 8 { return; }
    let hs = if focused { s_head() } else { s_subhead() };
    ColumnGrid::write_text(buf, area, area.y, &format!("tree ({})", app.dit_rows().len()), Align::Start, hs);
    hline(buf, Rect::new(area.x, area.y + 1, area.width, 1));

    let data = Rect::new(area.x, area.y + 2, area.width, area.height.saturating_sub(2));
    let vis  = data.height as usize;
    let cur  = &app.dit_cur;
    let content = vscroll(buf, data, cur.offset, app.dit_rows().len(), vis);
    let theme = mullion_theme();

    for (i, row) in app.dit_rows().iter().enumerate().skip(cur.offset).take(vis) {
        let y   = content.y + (i - cur.offset) as u16;
        let sel = i == cur.cursor;
        render_tree_row(
            buf, Rect::new(content.x, y, content.width, 1),
            &row.ancestor_last, row.is_last, row.expanded, &row.label, sel, &theme, dctx(),
        );
    }
}

fn render_entry(app: &App, buf: &mut Buffer, area: Rect, focused: bool) {
    if area.width < 10 { return; }
    let hs = if focused { s_head() } else { s_subhead() };
    ColumnGrid::write_text(buf, area, area.y, "attributes", Align::Start, hs);
    hline(buf, Rect::new(area.x, area.y + 1, area.width, 1));

    let attrs = app.dit_detail();
    if attrs.is_empty() {
        btxt(buf, area.x, area.y + 2, "(no entry selected)", s_dim());
        return;
    }
    // The cursored editable attribute (highlighted when this pane is focused).
    let editable = app.dit_edit_targets();
    let sel_attr = if focused { editable.get(app.dit_detail_cur).map(|(a, _)| a.clone()) } else { None };

    let data = Rect::new(area.x, area.y + 2, area.width, area.height.saturating_sub(2));
    let grid = kv_grid();
    let cols = grid.resolve(data);
    let max_y = data.y + data.height;
    let mut y = data.y;
    for (attr, vals) in attrs {
        for v in vals {
            if y >= max_y { return; }
            let is_sel = sel_attr.as_deref() == Some(attr.as_str());
            let (ks, vs) = if is_sel { (s_sel(), s_sel()) } else { (s_dim(), s_normal()) };
            if is_sel { fill_row(buf, data.x, y, data.width, s_sel()); }
            ColumnGrid::write_text(buf, cols[0], y, attr, Align::Start, ks);
            ColumnGrid::write_text_ctx(buf, cols[2], y, v, Align::Start, vs, dctx());
            y += 1;
        }
    }
}

fn kv_grid() -> ColumnGrid {
    ColumnGrid::new(vec![
        ColumnDef::fixed(20, ColumnKind::Text),
        ColumnDef::fixed(1,  ColumnKind::Custom),
        ColumnDef::fill(1,   ColumnKind::Text).with_min(8),
    ])
}
