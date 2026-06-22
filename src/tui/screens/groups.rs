//! Group screens: group picker and the two-pane membership editor.

use mullion::{
    border::Borders,
    label::Align,
    table::{ColumnDef, ColumnGrid, ColumnKind},
    Buffer, Rect,
};

use crate::ldap::client::User;
use crate::tui::app::App;
use crate::tui::draw::{btxt, fill_row, hline, inset};
use crate::tui::focus::Pane;
use crate::tui::theme::*;

pub fn render_select(app: &App, buf: &mut Buffer) {
    let area = buf.area;
    mullion::border::draw_box(buf, area, Borders::ALL, &box_style());
    btxt(buf, area.x + 2, area.y, "  census — select group  ", s_title());
    btxt(buf, area.x + 2, area.y + area.height - 1,
         " jk:scroll  Enter:manage  n:new  D:del  ?:help  Esc:cancel ", s_dim());

    let inner = inset(area, 1);
    if inner.height < 3 { return; }

    ColumnGrid::write_text(buf, inner, inner.y, "group", Align::Start, s_head());
    hline(buf, Rect::new(inner.x, inner.y + 1, inner.width, 1));

    let data = Rect::new(inner.x, inner.y + 2, inner.width, inner.height.saturating_sub(2));
    let vis  = data.height as usize;
    let cur  = &app.groups_cur;

    for (i, g) in app.groups().iter().enumerate().skip(cur.offset).take(vis) {
        let y   = data.y + (i - cur.offset) as u16;
        let sel = i == cur.cursor;
        let sty = if sel { s_sel() } else { s_normal() };
        if sel { fill_row(buf, inner.x, y, inner.width, sty); }

        let label = format!("{} ({} members)", g.name, g.members.len());
        ColumnGrid::write_text(buf, data, y, &label, Align::Start, sty);
    }
}

pub fn render_membership(app: &App, buf: &mut Buffer) {
    let area = buf.area;
    if area.width < 30 || area.height < 5 { return; }

    let gname = app.selected_group().map(|g| g.name.as_str()).unwrap_or("?");
    mullion::border::draw_box(buf, area, Borders::ALL, &box_style());
    btxt(buf, area.x + 2, area.y, &format!("  census — {gname}  "), s_title());

    let bottom = area.y + area.height - 1;
    match &app.status {
        Some((msg, is_err)) => {
            btxt(buf, area.x + 2, bottom, &format!(" {msg} "),
                 if *is_err { s_err() } else { s_ok() });
        }
        None => {
            let hints = if app.write_mode {
                " Tab:switch  Enter:add/remove  Esc:browse  q:quit "
            } else {
                " Tab:switch  Esc:browse  q:quit  (read-only) "
            };
            btxt(buf, area.x + 2, bottom, hints, s_dim());
        }
    }

    let inner = inset(area, 1);
    let mid   = inner.width / 2;
    let div_x = inner.x + mid;

    // Vertical divider
    btxt(buf, div_x, area.y, "┬", s_border());
    for y in inner.y..inner.y + inner.height {
        btxt(buf, div_x, y, "│", s_border());
    }
    btxt(buf, div_x, area.y + area.height - 1, "┴", s_border());

    let left_area  = Rect::new(inner.x, inner.y, mid,               inner.height);
    let right_area = Rect::new(div_x,   inner.y, inner.width - mid,  inner.height);
    let members    = app.member_list();

    render_user_pane(app, buf, left_area,  app.active_pane == Pane::Left);
    render_member_pane(app, buf, right_area, &members, app.active_pane == Pane::Right);
}

fn render_user_pane(app: &App, buf: &mut Buffer, area: Rect, active: bool) {
    if area.width < 10 { return; }
    let hs    = if active { s_head() } else { s_subhead() };
    let label = format!("all users ({})", app.users().len());
    ColumnGrid::write_text(buf, area, area.y, &label, Align::Start, hs);
    hline(buf, Rect::new(area.x, area.y + 1, area.width, 1));

    let cols = pair_grid().resolve(area);
    let data = Rect::new(area.x, area.y + 2, area.width, area.height.saturating_sub(2));
    let vis  = data.height as usize;
    let cur  = &app.left_cur;

    for (i, user) in app.users().iter().enumerate().skip(cur.offset).take(vis) {
        let y         = data.y + (i - cur.offset) as u16;
        let sel       = active && i == cur.cursor;
        let is_member = app.member_uids().iter().any(|uid| uid == &user.uid);
        let sty = if sel { s_sel() }
                  else if is_member { s_member() }
                  else if active { s_normal() }
                  else { s_dim() };

        if sel { fill_row(buf, area.x, y, area.width, sty); }
        ColumnGrid::write_text(buf, cols[0], y, &user.uid, Align::Start, sty);
        ColumnGrid::write_text(buf, cols[2], y, &user.cn,  Align::Start, sty);
    }
}

fn render_member_pane(app: &App, buf: &mut Buffer, area: Rect, members: &[&User], active: bool) {
    if area.width < 10 { return; }
    // skip divider character by starting one column right
    let content = Rect::new(area.x + 1, area.y, area.width.saturating_sub(1), area.height);
    let hs      = if active { s_head() } else { s_subhead() };
    let label   = format!("members ({})", members.len());
    ColumnGrid::write_text(buf, content, content.y, &label, Align::Start, hs);
    hline(buf, Rect::new(content.x, content.y + 1, content.width, 1));

    let cols = pair_grid().resolve(content);
    let data = Rect::new(content.x, content.y + 2, content.width, content.height.saturating_sub(2));
    let vis  = data.height as usize;
    let cur  = &app.right_cur;

    for (i, user) in members.iter().enumerate().skip(cur.offset).take(vis) {
        let y   = data.y + (i - cur.offset) as u16;
        let sel = active && i == cur.cursor;
        let sty = if sel { s_sel() } else if active { s_normal() } else { s_dim() };
        if sel { fill_row(buf, content.x, y, content.width, sty); }
        ColumnGrid::write_text(buf, cols[0], y, &user.uid, Align::Start, sty);
        ColumnGrid::write_text(buf, cols[2], y, &user.cn,  Align::Start, sty);
    }
}

fn pair_grid() -> ColumnGrid {
    ColumnGrid::new(vec![
        ColumnDef::fixed(12, ColumnKind::Text),              // uid
        ColumnDef::fixed(1,  ColumnKind::Custom),
        ColumnDef::fill(1,   ColumnKind::Text).with_min(8),  // name
    ])
}
