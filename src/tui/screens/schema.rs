//! Schema browser: every `attributeType` / `objectClass` the server publishes (left,
//! filterable) beside the selected definition (right). Read-only; works on any brand.

use mullion::{
    label::Align, render_shared,
    table::{ColumnDef, ColumnGrid, ColumnKind},
    Buffer, Constraint, LineWeight, Node, Orientation, Rect, Size,
};

use crate::ldap::client::SchemaKind;
use crate::tui::app::App;
use crate::tui::draw::{btxt, fill_row, hline, keyhints, vscroll};
use crate::tui::focus::Pane;
use crate::tui::theme::*;

const LIST: u64 = 1;
const DEFN: u64 = 2;

pub fn render(app: &App, buf: &mut Buffer, area: Rect) {
    if area.width < 20 || area.height < 5 { return; }
    let focus = app.schema_pane();

    let mut tree = Node::Split {
        orientation: Orientation::Horizontal,
        children: vec![
            (Constraint::new(Size::Percent(42)).with_min(24), Node::Tile(LIST)),
            (Constraint::new(Size::Fill(1)), Node::Tile(DEFN)),
        ],
    };
    let focused = if focus == Pane::Left { LIST } else { DEFN };
    let rects = render_shared(buf, &mut tree, area, &box_style(), &[(focused, LineWeight::Heavy)]);

    btxt(buf, area.x + 2, area.y, &format!("  census — schema: {}  ", app.schema_title()), s_title());

    let bottom = area.y + area.height - 1;
    match &app.status {
        Some((msg, is_err)) =>
            btxt(buf, area.x + 2, bottom, &format!(" {msg} "), if *is_err { s_err() } else { s_ok() }),
        None => {
            let pairs: &[(&str, &str)] = &[("jk", "nav"), ("/", "filter"), ("Tab", "defn"),
                                           ("Esc", "back"), ("q", "quit")];
            keyhints(buf, area.x + 2, bottom, area.width.saturating_sub(4), pairs);
        }
    }

    for (id, r) in rects {
        match id {
            LIST => render_list(app, buf, r, focus == Pane::Left),
            DEFN => render_defn(app, buf, r, focus == Pane::Right),
            _ => {}
        }
    }
}

fn render_list(app: &App, buf: &mut Buffer, area: Rect, focused: bool) {
    if area.width < 8 { return; }
    let hs = if focused { s_head() } else { s_subhead() };
    let rows = app.schema_rows();

    // Header: the live filter (with a caret while editing), else a count + hint.
    let header = if app.schema_filtering() || !app.schema_filter().is_empty() {
        let caret = if app.schema_filtering() { "▏" } else { "" };
        format!("filter: {}{caret}", app.schema_filter())
    } else {
        format!("{} of {} defs  (/ filter)", rows.len(), app.schema_elems().len())
    };
    ColumnGrid::write_text(buf, area, area.y, &header, Align::Start, hs);
    hline(buf, Rect::new(area.x, area.y + 1, area.width, 1));

    let data = Rect::new(area.x, area.y + 2, area.width, area.height.saturating_sub(2));
    let vis = data.height as usize;
    let cur = app.schema_cursor();
    let content = vscroll(buf, data, cur.offset, rows.len(), vis);
    let cols = list_grid().resolve(content);
    let elems = app.schema_elems();

    for (row, &ei) in rows.iter().enumerate().skip(cur.offset).take(vis) {
        let e = &elems[ei];
        let y = content.y + (row - cur.offset) as u16;
        let sel = row == cur.cursor;
        let sty = if sel { s_sel() } else if focused { s_normal() } else { s_dim() };
        if sel { fill_row(buf, content.x, y, content.width, sty); }
        // 'A' attributeType / 'O' objectClass, then the name.
        let mk = e.kind.marker().to_string();
        ColumnGrid::write_text(buf, cols[0], y, &mk, Align::Start, if sel { sty } else { s_dim() });
        ColumnGrid::write_text(buf, cols[2], y, &e.name, Align::Start, sty);
    }
}

fn list_grid() -> ColumnGrid {
    ColumnGrid::new(vec![
        ColumnDef::fixed(1, ColumnKind::Text),               // A/O marker
        ColumnDef::fixed(1, ColumnKind::Custom),             // gap
        ColumnDef::fill(1, ColumnKind::Text).with_min(6),    // name
    ])
}

fn render_defn(app: &App, buf: &mut Buffer, area: Rect, focused: bool) {
    if area.width < 10 { return; }
    let hs = if focused { s_head() } else { s_subhead() };
    ColumnGrid::write_text(buf, area, area.y, "definition", Align::Start, hs);
    hline(buf, Rect::new(area.x, area.y + 1, area.width, 1));

    let rows = app.schema_rows();
    let cur = app.schema_cursor();
    let data = Rect::new(area.x, area.y + 2, area.width, area.height.saturating_sub(2));
    let Some(&ei) = rows.get(cur.cursor) else {
        btxt(buf, data.x, data.y, "(no match)", s_dim());
        return;
    };
    let e = &app.schema_elems()[ei];
    let kind = match e.kind { SchemaKind::Attribute => "attributeType", SchemaKind::ObjectClass => "objectClass" };
    btxt(buf, data.x, data.y, &format!("{}  ·  {kind}  ·  {}", e.name, e.oid), s_subhead());

    let maxy = data.y + data.height;
    for (y, line) in (data.y + 2..).zip(wrap(&e.raw, data.width as usize)) {
        if y >= maxy { break; }
        btxt(buf, data.x, y, &line, s_normal());
    }
}

/// Naive whitespace word-wrap for the (single-line) definition string.
fn wrap(s: &str, w: usize) -> Vec<String> {
    if w == 0 { return Vec::new(); }
    let mut out = Vec::new();
    let mut cur = String::new();
    for word in s.split_whitespace() {
        if cur.is_empty() {
            cur = word.to_string();
        } else if cur.chars().count() + 1 + word.chars().count() <= w {
            cur.push(' ');
            cur.push_str(word);
        } else {
            out.push(std::mem::take(&mut cur));
            cur = word.to_string();
        }
    }
    if !cur.is_empty() { out.push(cur); }
    out
}
