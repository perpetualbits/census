//! Browse screen: full-width user table (uid, name, shell, groups).

use mullion::{
    border::Borders,
    label::Align,
    table::{ColumnDef, ColumnGrid, ColumnKind},
    Buffer, Rect,
};

use crate::tui::app::App;
use crate::tui::draw::{btxt, fill_row, hline, inset};
use crate::tui::theme::*;

pub fn render(app: &App, buf: &mut Buffer) {
    let area = buf.area;
    if area.width < 20 || area.height < 5 { return; }

    mullion::border::draw_box(buf, area, Borders::ALL, &box_style());
    btxt(buf, area.x + 2, area.y, "  census  ", s_title());
    btxt(buf, area.x + 2, area.y + area.height - 1,
         " g:groups  ↑↓/jk:scroll  q:quit ", s_dim());

    let bottom = area.y + area.height - 1;
    match &app.status {
        Some((msg, is_err)) => {
            let sx = area.x + area.width / 2;
            btxt(buf, sx, bottom, &format!(" {msg} "), if *is_err { s_err() } else { s_ok() });
        }
        None => {
            let count = format!(" {} users ", app.users().len());
            let cx = area.x + area.width.saturating_sub(1 + count.len() as u16);
            btxt(buf, cx, bottom, &count, s_dim());
        }
    }

    let inner = inset(area, 1);
    if inner.height < 3 { return; }

    let grid = user_grid();
    let cols = grid.resolve(inner);

    let hy = inner.y;
    ColumnGrid::write_text(buf, cols[0], hy, "uid",    Align::Start, s_head());
    ColumnGrid::write_text(buf, cols[2], hy, "name",   Align::Start, s_head());
    ColumnGrid::write_text(buf, cols[4], hy, "shell",  Align::Start, s_head());
    ColumnGrid::write_text(buf, cols[6], hy, "groups", Align::Start, s_head());
    hline(buf, Rect::new(inner.x, inner.y + 1, inner.width, 1));

    let data = Rect::new(inner.x, inner.y + 2, inner.width, inner.height.saturating_sub(2));
    let vis  = data.height as usize;
    let cur  = &app.users_cur;

    for (i, user) in app.users().iter().enumerate().skip(cur.offset).take(vis) {
        let y   = data.y + (i - cur.offset) as u16;
        let sel = i == cur.cursor;
        let sty = if sel { s_sel() } else { s_normal() };
        if sel { fill_row(buf, inner.x, y, inner.width, sty); }

        let shell = user.shell.rsplit('/').next().unwrap_or(&user.shell);
        let grps  = app.user_groups().get(i).map(|g| g.join(", ")).unwrap_or_default();
        ColumnGrid::write_text(buf, cols[0], y, &user.uid, Align::Start, sty);
        ColumnGrid::write_text(buf, cols[2], y, &user.cn,  Align::Start, sty);
        ColumnGrid::write_text(buf, cols[4], y, shell,     Align::Start, sty);
        ColumnGrid::write_text(buf, cols[6], y, &grps,     Align::Start, sty);
    }
}

fn user_grid() -> ColumnGrid {
    ColumnGrid::new(vec![
        ColumnDef::fixed(12, ColumnKind::Text),              // uid
        ColumnDef::fixed(1,  ColumnKind::Custom),            // gap
        ColumnDef::fill(3,   ColumnKind::Text).with_min(16), // name
        ColumnDef::fixed(1,  ColumnKind::Custom),
        ColumnDef::fixed(8,  ColumnKind::Text),              // shell
        ColumnDef::fixed(1,  ColumnKind::Custom),
        ColumnDef::fill(2,   ColumnKind::Text).with_min(12), // groups
    ])
}
