//! Group screens: group picker and the two-pane membership editor.

use mullion::{
    label::Align,
    render_shared,
    table::{ColumnDef, ColumnGrid, ColumnKind},
    Buffer, Constraint, LineWeight, Node, Orientation, Rect, Size,
};

use crate::ldap::client::User;
use crate::tui::app::App;
use crate::tui::draw::{btxt, fill_row, hline, keyhints, vscroll};
use crate::tui::focus::Pane;
use crate::tui::theme::*;

/// Stable tile ids for the group screens.
const GROUP_LIST:   u64 = 1;
const GROUP_DETAIL: u64 = 2;
const ALL_USERS:    u64 = 1;
const MEMBERS:      u64 = 2;

/// The group browse screen: group list (left) beside the per-group detail pane
/// (right), mirroring the user browse screen. `Enter` on the list opens membership.
pub fn render_select(app: &App, buf: &mut Buffer, area: Rect) {
    if area.width < 20 || area.height < 5 { return; }
    let focus = app.group_browse_focus;

    let mut tree = Node::Split {
        orientation: Orientation::Horizontal,
        children: vec![
            (Constraint::new(Size::Percent(45)).with_min(24).with_max(56), Node::Tile(GROUP_LIST)),
            (Constraint::new(Size::Fill(1)), Node::Tile(GROUP_DETAIL)),
        ],
    };
    let focused = if focus == Pane::Left { GROUP_LIST } else { GROUP_DETAIL };
    let rects = render_shared(buf, &mut tree, area, &box_style(), &[(focused, LineWeight::Heavy)]);

    btxt(buf, area.x + 2, area.y, "  census — groups  ", s_title());

    // Duplicate summary on the top border, right-aligned, when collisions exist.
    let n_dup_gid  = app.groups().iter().filter(|g| g.dup_gid).count();
    let n_dup_name = app.groups().iter().filter(|g| g.dup_name).count();
    if n_dup_gid + n_dup_name > 0 {
        let summary = format!(" ⚠ {n_dup_gid} dup-gid · {n_dup_name} dup-name ");
        let sx = area.x + area.width.saturating_sub(1 + summary.chars().count() as u16);
        btxt(buf, sx, area.y, &summary, s_warn());
    }

    let bottom = area.y + area.height - 1;
    match &app.status {
        Some((msg, is_err)) => {
            btxt(buf, area.x + 2, bottom, &format!(" {msg} "),
                 if *is_err { s_err() } else { s_ok() });
        }
        None => {
            let pairs: &[(&str, &str)] = if focus == Pane::Right {
                &[("Tab", "list"), ("jk", "attr"), ("e", "edit"), ("E", "big-edit"), ("r", "rename"),
                  ("a", "del-alias"), ("u", "undo"), ("?", "help"), ("Esc", "")]
            } else {
                &[("Tab", "detail"), ("jk", ""), ("/", "search"), ("Enter", "members"), ("n", "new"),
                  ("D", "del"), ("a", "alias"), ("u", "undo"), ("L", "ldif"), ("?", "help"), ("Esc", "")]
            };
            keyhints(buf, area.x + 2, bottom, area.width.saturating_sub(4), pairs);
        }
    }

    for (id, r) in rects {
        match id {
            GROUP_LIST   => render_group_list(app, buf, r, focus == Pane::Left),
            GROUP_DETAIL => super::group_detail::render(app, buf, r, focus == Pane::Right),
            _ => {}
        }
    }
}

fn render_group_list(app: &App, buf: &mut Buffer, inner: Rect, focused: bool) {
    if inner.height < 3 { return; }
    let hs = if focused { s_head() } else { s_subhead() };
    let label = if app.groups_truncated() {
        format!("groups ({} — capped)", app.groups().len())
    } else {
        format!("groups ({})", app.groups().len())
    };
    ColumnGrid::write_text(buf, inner, inner.y, &label, Align::Start, hs);
    hline(buf, Rect::new(inner.x, inner.y + 1, inner.width, 1));

    let data = Rect::new(inner.x, inner.y + 2, inner.width, inner.height.saturating_sub(2));
    let vis  = data.height as usize;
    let cur  = &app.groups_cur;
    let content = vscroll(buf, data, cur.offset, app.groups().len(), vis);

    for (i, g) in app.groups().iter().enumerate().skip(cur.offset).take(vis) {
        let y   = content.y + (i - cur.offset) as u16;
        let sel = i == cur.cursor;
        let dup = g.dup_name || g.dup_gid;
        let sty = if sel { s_sel() }
                  else if dup { s_warn() }
                  else if focused { s_normal() }
                  else { s_dim() };
        if sel { fill_row(buf, content.x, y, content.width, sty); }

        let gid = g.gid_number.map(|n| n.to_string()).unwrap_or_else(|| "—".into());
        let alias = if g.aliases.is_empty() {
            String::new()
        } else {
            format!("  aka {}", g.aliases.join(","))
        };
        // No member count: members aren't loaded for the list (a group can hold
        // millions of members — see list_groups). Membership editor loads them per-group.
        let label = format!("{}  gid {}{}", g.name, gid, alias);
        ColumnGrid::write_text(buf, content, y, &label, Align::Start, sty);

        // Right-aligned collision marker, so duplicates are unmistakable.
        if dup {
            let mut marks = Vec::new();
            if g.dup_name { marks.push("dup-name"); }
            if g.dup_gid  { marks.push("dup-gid"); }
            let mark = format!("⚠ {} ", marks.join(" "));
            let mx = content.x + content.width.saturating_sub(mark.chars().count() as u16);
            btxt(buf, mx, y, &mark, if sel { s_sel() } else { s_warn() });
        }
    }
}

pub fn render_membership(app: &App, buf: &mut Buffer, area: Rect) {
    if area.width < 30 || area.height < 5 { return; }

    let gname = app.selected_group().map(|g| g.name.as_str()).unwrap_or("?");

    // All-users pane beside the members pane, split down the middle. The engine
    // draws the frame and the shared divider; the active pane is heavied.
    let mut tree = Node::Split {
        orientation: Orientation::Horizontal,
        children: vec![
            (Constraint::new(Size::Fill(1)), Node::Tile(ALL_USERS)),
            (Constraint::new(Size::Fill(1)), Node::Tile(MEMBERS)),
        ],
    };
    let focused = if app.active_pane == Pane::Left { ALL_USERS } else { MEMBERS };
    let rects = render_shared(
        buf, &mut tree, area, &box_style(),
        &[(focused, LineWeight::Heavy)],
    );

    btxt(buf, area.x + 2, area.y, &format!("  census — {gname}  "), s_title());

    let bottom = area.y + area.height - 1;
    match &app.status {
        Some((msg, is_err)) => {
            btxt(buf, area.x + 2, bottom, &format!(" {msg} "),
                 if *is_err { s_err() } else { s_ok() });
        }
        None => {
            let pairs: &[(&str, &str)] = if app.can_write_ui() {
                &[("Tab", "switch"), ("Enter", "add/remove"), ("u", "undo"),
                  ("L", "ldif"), ("Esc", "browse"), ("q", "quit")]
            } else {
                &[("Tab", "switch"), ("L", "ldif"), ("Esc", "browse"),
                  ("q", "quit"), ("", "(read-only)")]
            };
            keyhints(buf, area.x + 2, bottom, area.width.saturating_sub(4), pairs);
        }
    }

    let members = app.member_list();
    for (id, r) in rects {
        match id {
            ALL_USERS => render_user_pane(app, buf, r, app.active_pane == Pane::Left),
            MEMBERS   => render_member_pane(app, buf, r, &members, app.active_pane == Pane::Right),
            _ => {}
        }
    }
}

fn render_user_pane(app: &App, buf: &mut Buffer, area: Rect, active: bool) {
    if area.width < 10 { return; }
    let hs    = if active { s_head() } else { s_subhead() };
    let label = format!("all users ({})", app.users().len());
    ColumnGrid::write_text(buf, area, area.y, &label, Align::Start, hs);
    hline(buf, Rect::new(area.x, area.y + 1, area.width, 1));

    let data = Rect::new(area.x, area.y + 2, area.width, area.height.saturating_sub(2));
    let vis  = data.height as usize;
    let cur  = &app.left_cur;
    let content = vscroll(buf, data, cur.offset, app.users().len(), vis);
    let cols = pair_grid().resolve(content);

    for (i, user) in app.users().iter().enumerate().skip(cur.offset).take(vis) {
        let y         = content.y + (i - cur.offset) as u16;
        let sel       = active && i == cur.cursor;
        let is_member = app.member_uids().iter().any(|uid| uid == &user.uid);
        let sty = if sel { s_sel() }
                  else if is_member { s_member() }
                  else if active { s_normal() }
                  else { s_dim() };

        if sel { fill_row(buf, content.x, y, content.width, sty); }
        ColumnGrid::write_text(buf, cols[0], y, &user.uid, Align::Start, sty);
        ColumnGrid::write_text_ctx(buf, cols[2], y, &user.cn, Align::Start, sty, dctx());
    }
}

fn render_member_pane(app: &App, buf: &mut Buffer, content: Rect, members: &[&User], active: bool) {
    if content.width < 10 { return; }
    let hs      = if active { s_head() } else { s_subhead() };
    let label   = format!("members ({})", members.len());
    ColumnGrid::write_text(buf, content, content.y, &label, Align::Start, hs);
    hline(buf, Rect::new(content.x, content.y + 1, content.width, 1));

    let data = Rect::new(content.x, content.y + 2, content.width, content.height.saturating_sub(2));
    let vis  = data.height as usize;
    let cur  = &app.right_cur;
    let rows = vscroll(buf, data, cur.offset, members.len(), vis);
    let cols = pair_grid().resolve(rows);

    for (i, user) in members.iter().enumerate().skip(cur.offset).take(vis) {
        let y   = rows.y + (i - cur.offset) as u16;
        let sel = active && i == cur.cursor;
        let sty = if sel { s_sel() } else if active { s_normal() } else { s_dim() };
        if sel { fill_row(buf, rows.x, y, rows.width, sty); }
        ColumnGrid::write_text(buf, cols[0], y, &user.uid, Align::Start, sty);
        ColumnGrid::write_text_ctx(buf, cols[2], y, &user.cn, Align::Start, sty, dctx());
    }
}

fn pair_grid() -> ColumnGrid {
    ColumnGrid::new(vec![
        ColumnDef::fixed(12, ColumnKind::Text),              // uid
        ColumnDef::fixed(1,  ColumnKind::Custom),
        ColumnDef::fill(1,   ColumnKind::Text).with_min(8),  // name
    ])
}
