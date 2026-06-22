//! Browse screen: user list (left) beside a per-user detail pane (right).

use mullion::{
    border::Borders,
    label::Align,
    table::{ColumnDef, ColumnGrid, ColumnKind},
    Buffer, Rect,
};

use crate::tui::app::App;
use crate::tui::draw::{btxt, fill_row, hline, inset};
use crate::tui::focus::Pane;
use crate::tui::theme::*;

use super::detail;

pub fn render(app: &App, buf: &mut Buffer, focus: Pane) {
    let area = buf.area;
    if area.width < 20 || area.height < 5 { return; }

    mullion::border::draw_box(buf, area, Borders::ALL, &box_style());
    btxt(buf, area.x + 2, area.y, "  census  ", s_title());

    let bottom = area.y + area.height - 1;
    match &app.status {
        Some((msg, is_err)) => {
            btxt(buf, area.x + 2, bottom, &format!(" {msg} "),
                 if *is_err { s_err() } else { s_ok() });
        }
        None => {
            let hint = if focus == Pane::Right {
                " Tab:pane  ↑↓/jk:attr  e:edit  K:keys  g:groups  q:quit "
            } else {
                " Tab:pane  g:groups  ↑↓/jk:users  q:quit "
            };
            btxt(buf, area.x + 2, bottom, hint, s_dim());
            let count = format!(" {} users ", app.users().len());
            let cx = area.x + area.width.saturating_sub(1 + count.len() as u16);
            btxt(buf, cx, bottom, &count, s_dim());
        }
    }

    let inner = inset(area, 1);
    if inner.height < 3 || inner.width < 12 { return; }

    // Split: list pane on the left, detail on the right.
    let list_w = (inner.width * 2 / 5).clamp(20, 44).min(inner.width.saturating_sub(12));
    let div_x  = inner.x + list_w;

    btxt(buf, div_x, area.y, "┬", s_border());
    for y in inner.y..inner.y + inner.height {
        btxt(buf, div_x, y, "│", s_border());
    }
    btxt(buf, div_x, area.y + area.height - 1, "┴", s_border());

    let list_area   = Rect::new(inner.x, inner.y, list_w, inner.height);
    let detail_area = Rect::new(div_x + 1, inner.y, inner.width.saturating_sub(list_w + 1), inner.height);

    render_list(app, buf, list_area, focus == Pane::Left);
    detail::render(app, buf, detail_area, focus == Pane::Right);
}

fn render_list(app: &App, buf: &mut Buffer, area: Rect, focused: bool) {
    let hs    = if focused { s_head() } else { s_subhead() };
    let label = format!("users ({})", app.users().len());
    ColumnGrid::write_text(buf, area, area.y, &label, Align::Start, hs);
    hline(buf, Rect::new(area.x, area.y + 1, area.width, 1));

    let cols = list_grid().resolve(area);
    let data = Rect::new(area.x, area.y + 2, area.width, area.height.saturating_sub(2));
    let vis  = data.height as usize;
    let cur  = &app.users_cur;

    for (i, user) in app.users().iter().enumerate().skip(cur.offset).take(vis) {
        let y   = data.y + (i - cur.offset) as u16;
        let sel = i == cur.cursor;
        let sty = if sel { s_sel() } else if focused { s_normal() } else { s_dim() };
        if sel { fill_row(buf, area.x, y, area.width, sty); }
        ColumnGrid::write_text(buf, cols[0], y, &user.uid, Align::Start, sty);
        ColumnGrid::write_text(buf, cols[2], y, &user.cn,  Align::Start, sty);
    }
}

fn list_grid() -> ColumnGrid {
    ColumnGrid::new(vec![
        ColumnDef::fixed(12, ColumnKind::Text),             // uid
        ColumnDef::fixed(1,  ColumnKind::Custom),           // gap
        ColumnDef::fill(1,   ColumnKind::Text).with_min(6), // name
    ])
}
