use std::time::Duration;

use crossterm::event::Event;
use mullion::{
    backend::CrosstermBackend,
    border::{draw_box, BorderStyle, Borders, CornerStyle, LineWeight},
    label::Align,
    poll_event,
    style::{Color, Modifier, Style},
    table::{ColumnDef, ColumnGrid, ColumnKind},
    Buffer, KeyCode, KeyModifiers, Rect, Terminal,
};

use crate::config::Config;
use crate::ldap::client::{Group, LdapClient, User};

// ─── palette ─────────────────────────────────────────────────────────────────

const C_BORDER: Color = Color::Rgb(70,  70,  100);
const C_FG:     Color = Color::Rgb(200, 200, 210);
const C_DIM:    Color = Color::Rgb(110, 110, 130);
const C_HEAD:   Color = Color::Rgb(255, 255, 255);
const C_HDR2:   Color = Color::Rgb(140, 170, 255);
const C_TITLE:  Color = Color::Rgb(160, 160, 255);
const C_SEL_FG: Color = Color::Rgb(0,   0,   0  );
const C_SEL_BG: Color = Color::Rgb(80,  120, 210);
const C_MEMBER: Color = Color::Rgb(80,  190, 100);
const C_OK:     Color = Color::Rgb(80,  200, 100);
const C_ERR:    Color = Color::Rgb(220, 80,  80 );

fn s_border()  -> Style { Style::default().fg(C_BORDER) }
fn s_normal()  -> Style { Style::default().fg(C_FG) }
fn s_dim()     -> Style { Style::default().fg(C_DIM) }
fn s_title()   -> Style { Style::default().fg(C_TITLE) }
fn s_head()    -> Style { Style::default().fg(C_HEAD).add_modifier(Modifier::BOLD) }
fn s_subhead() -> Style { Style::default().fg(C_HDR2) }
fn s_sel()     -> Style { Style::default().fg(C_SEL_FG).bg(C_SEL_BG) }
fn s_member()  -> Style { Style::default().fg(C_MEMBER) }
fn s_ok()      -> Style { Style::default().fg(C_OK) }
fn s_err()     -> Style { Style::default().fg(C_ERR) }

fn box_style() -> BorderStyle {
    BorderStyle { weight: LineWeight::Light, corners: CornerStyle::Rounded, style: s_border() }
}

// ─── state ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode { Browse, GroupSelect, Membership }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pane { Left, Right }

struct App {
    users:  Vec<User>,
    groups: Vec<Group>,
    mode:   Mode,

    user_offset: usize,
    user_cursor: usize,

    group_offset: usize,
    group_cursor: usize,

    selected_group: usize,
    active_pane: Pane,
    left_offset:  usize,
    left_cursor:  usize,
    right_offset: usize,
    right_cursor: usize,

    status:     Option<(String, bool)>,
    write_mode: bool,

    // precomputed: user index → group names that user belongs to
    user_groups: Vec<Vec<String>>,
}

impl App {
    fn new(users: Vec<User>, groups: Vec<Group>, write_mode: bool) -> Self {
        let user_groups = build_user_groups(&users, &groups);
        Self {
            users, groups, mode: Mode::Browse,
            user_offset: 0, user_cursor: 0,
            group_offset: 0, group_cursor: 0,
            selected_group: 0,
            active_pane: Pane::Left,
            left_offset: 0, left_cursor: 0,
            right_offset: 0, right_cursor: 0,
            status: None, write_mode, user_groups,
        }
    }

    fn refresh_groups(&mut self, client: &mut LdapClient) -> anyhow::Result<()> {
        self.groups = client.list_groups()?;
        self.user_groups = build_user_groups(&self.users, &self.groups);
        Ok(())
    }

    fn selected_group(&self) -> Option<&Group> {
        self.groups.get(self.selected_group)
    }

    fn member_uids(&self) -> &[String] {
        self.selected_group().map(|g| g.members.as_slice()).unwrap_or(&[])
    }
}

fn build_user_groups(users: &[User], groups: &[Group]) -> Vec<Vec<String>> {
    users.iter().map(|u| {
        groups.iter()
            .filter(|g| g.members.iter().any(|m| m == &u.uid))
            .map(|g| g.name.clone())
            .collect()
    }).collect()
}

fn member_list<'a>(app: &'a App) -> Vec<&'a User> {
    app.member_uids().iter()
        .filter_map(|uid| app.users.iter().find(|u| &u.uid == uid))
        .collect()
}

// ─── entry point ─────────────────────────────────────────────────────────────

pub fn run(client: &mut LdapClient, _cfg: &Config, write_mode: bool) -> anyhow::Result<()> {
    let users  = client.list_users()?;
    let groups = client.list_groups()?;
    let mut app = App::new(users, groups, write_mode);

    let mut term = Terminal::new(CrosstermBackend::new(std::io::stdout()))?;
    term.enter()?;
    let result = main_loop(&mut term, &mut app, client);
    term.leave()?;
    result
}

// ─── event loop ──────────────────────────────────────────────────────────────

fn main_loop(
    term:   &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    app:    &mut App,
    client: &mut LdapClient,
) -> anyhow::Result<()> {
    loop {
        term.draw(|buf| {
            update_offsets(app, buf.area);
            render(app, buf);
        })?;

        match poll_event(Duration::from_millis(100))? {
            None => continue,
            Some(Event::Key(key)) => {
                if handle_key(app, client, key.code, key.modifiers)? {
                    return Ok(());
                }
            }
            Some(Event::Resize(..)) => {}
            Some(_) => {}
        }
    }
}

// ─── scroll helper ───────────────────────────────────────────────────────────

fn keep_in_view(offset: usize, cursor: usize, visible: usize) -> usize {
    if visible == 0 { return 0; }
    if cursor < offset { cursor }
    else if cursor >= offset + visible { cursor + 1 - visible }
    else { offset }
}

fn update_offsets(app: &mut App, area: Rect) {
    // border(2) + header(1) + sep(1) = 4
    let vis = area.height.saturating_sub(4) as usize;
    if vis == 0 { return; }

    app.user_offset  = keep_in_view(app.user_offset,  app.user_cursor,  vis);
    app.group_offset = keep_in_view(app.group_offset, app.group_cursor, vis);
    app.left_offset  = keep_in_view(app.left_offset,  app.left_cursor,  vis);

    let rlen = app.member_uids().len();
    if rlen > 0 && app.right_cursor >= rlen { app.right_cursor = rlen - 1; }
    app.right_offset = keep_in_view(app.right_offset, app.right_cursor, vis);
}

// ─── key handling ────────────────────────────────────────────────────────────

fn handle_key(
    app:    &mut App,
    client: &mut LdapClient,
    key:    KeyCode,
    mods:   KeyModifiers,
) -> anyhow::Result<bool> {
    use KeyCode::*;

    if key == Char('c') && mods.contains(KeyModifiers::CONTROL) { return Ok(true); }

    app.status = None;

    match app.mode {
        Mode::Browse => match key {
            Char('q') | Esc => return Ok(true),
            Char('g') => { app.mode = Mode::GroupSelect; app.group_cursor = 0; app.group_offset = 0; }
            Up   | Char('k') => { app.user_cursor = app.user_cursor.saturating_sub(1); }
            Down | Char('j') => { if app.user_cursor + 1 < app.users.len() { app.user_cursor += 1; } }
            PageUp   => { app.user_cursor = app.user_cursor.saturating_sub(10); }
            PageDown => { app.user_cursor = (app.user_cursor + 10).min(app.users.len().saturating_sub(1)); }
            _ => {}
        },

        Mode::GroupSelect => match key {
            Esc => { app.mode = Mode::Browse; }
            Up   | Char('k') => { app.group_cursor = app.group_cursor.saturating_sub(1); }
            Down | Char('j') => { if app.group_cursor + 1 < app.groups.len() { app.group_cursor += 1; } }
            Enter => {
                app.selected_group = app.group_cursor;
                app.mode = Mode::Membership;
                app.active_pane = Pane::Left;
                app.left_cursor = 0; app.left_offset = 0;
                app.right_cursor = 0; app.right_offset = 0;
            }
            _ => {}
        },

        Mode::Membership => match key {
            Char('q') => return Ok(true),
            Esc => { app.mode = Mode::Browse; }
            Tab | BackTab => {
                app.active_pane = if app.active_pane == Pane::Left { Pane::Right } else { Pane::Left };
            }
            Up | Char('k') => match app.active_pane {
                Pane::Left  => { app.left_cursor  = app.left_cursor.saturating_sub(1); }
                Pane::Right => { app.right_cursor = app.right_cursor.saturating_sub(1); }
            },
            Down | Char('j') => match app.active_pane {
                Pane::Left => {
                    if app.left_cursor + 1 < app.users.len() { app.left_cursor += 1; }
                }
                Pane::Right => {
                    let n = member_list(app).len();
                    if app.right_cursor + 1 < n { app.right_cursor += 1; }
                }
            },
            Enter => {
                if !app.write_mode {
                    app.status = Some(("Read-only — pass --write to modify".into(), true));
                } else {
                    do_membership_action(app, client)?;
                }
            }
            _ => {}
        },
    }
    Ok(false)
}

fn do_membership_action(app: &mut App, client: &mut LdapClient) -> anyhow::Result<()> {
    match app.active_pane {
        Pane::Left => {
            let uid  = app.users[app.left_cursor].uid.clone();
            let dn   = app.groups[app.selected_group].dn.clone();
            let name = app.groups[app.selected_group].name.clone();
            if app.member_uids().iter().any(|m| m == &uid) {
                app.status = Some((format!("{uid} is already in {name}"), false));
            } else {
                match client.group_add_member(&dn, &uid) {
                    Ok(()) => {
                        app.refresh_groups(client)?;
                        app.status = Some((format!("Added {uid} to {name}"), false));
                    }
                    Err(e) => { app.status = Some((format!("Error: {e}"), true)); }
                }
            }
        }
        Pane::Right => {
            let members = member_list(app);
            if let Some(user) = members.get(app.right_cursor) {
                let uid  = user.uid.clone();
                let dn   = app.groups[app.selected_group].dn.clone();
                let name = app.groups[app.selected_group].name.clone();
                match client.group_remove_member(&dn, &uid) {
                    Ok(()) => {
                        app.refresh_groups(client)?;
                        app.status = Some((format!("Removed {uid} from {name}"), false));
                    }
                    Err(e) => { app.status = Some((format!("Error: {e}"), true)); }
                }
            }
        }
    }
    Ok(())
}

// ─── render dispatch ─────────────────────────────────────────────────────────

fn render(app: &App, buf: &mut Buffer) {
    match app.mode {
        Mode::Browse      => render_browse(app, buf),
        Mode::GroupSelect => render_group_select(app, buf),
        Mode::Membership  => render_membership(app, buf),
    }
}

fn btxt(buf: &mut Buffer, x: u16, y: u16, text: &str, style: Style) {
    buf.set_string(x, y, text, style);
}

// ─── browse ──────────────────────────────────────────────────────────────────

fn render_browse(app: &App, buf: &mut Buffer) {
    let area = buf.area;
    if area.width < 20 || area.height < 5 { return; }

    draw_box(buf, area, Borders::ALL, &box_style());
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
            let count = format!(" {} users ", app.users.len());
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

    for (i, user) in app.users.iter().enumerate().skip(app.user_offset).take(vis) {
        let y   = data.y + (i - app.user_offset) as u16;
        let sel = i == app.user_cursor;
        let sty = if sel { s_sel() } else { s_normal() };
        if sel { fill_row(buf, inner.x, y, inner.width, sty); }

        let shell = user.shell.rsplit('/').next().unwrap_or(&user.shell);
        let grps  = app.user_groups.get(i).map(|g| g.join(", ")).unwrap_or_default();
        ColumnGrid::write_text(buf, cols[0], y, &user.uid, Align::Start, sty);
        ColumnGrid::write_text(buf, cols[2], y, &user.cn,  Align::Start, sty);
        ColumnGrid::write_text(buf, cols[4], y, shell,     Align::Start, sty);
        ColumnGrid::write_text(buf, cols[6], y, &grps,     Align::Start, sty);
    }
}

// ─── group select ────────────────────────────────────────────────────────────

fn render_group_select(app: &App, buf: &mut Buffer) {
    let area = buf.area;
    draw_box(buf, area, Borders::ALL, &box_style());
    btxt(buf, area.x + 2, area.y, "  census — select group  ", s_title());
    btxt(buf, area.x + 2, area.y + area.height - 1,
         " ↑↓/jk:scroll  Enter:manage  Esc:cancel ", s_dim());

    let inner = inset(area, 1);
    if inner.height < 3 { return; }

    ColumnGrid::write_text(buf, inner, inner.y, "group", Align::Start, s_head());
    hline(buf, Rect::new(inner.x, inner.y + 1, inner.width, 1));

    let data = Rect::new(inner.x, inner.y + 2, inner.width, inner.height.saturating_sub(2));
    let vis  = data.height as usize;

    for (i, g) in app.groups.iter().enumerate().skip(app.group_offset).take(vis) {
        let y   = data.y + (i - app.group_offset) as u16;
        let sel = i == app.group_cursor;
        let sty = if sel { s_sel() } else { s_normal() };
        if sel { fill_row(buf, inner.x, y, inner.width, sty); }

        let label = format!("{} ({} members)", g.name, g.members.len());
        ColumnGrid::write_text(buf, data, y, &label, Align::Start, sty);
    }
}

// ─── membership ──────────────────────────────────────────────────────────────

fn render_membership(app: &App, buf: &mut Buffer) {
    let area = buf.area;
    if area.width < 30 || area.height < 5 { return; }

    let gname = app.selected_group().map(|g| g.name.as_str()).unwrap_or("?");
    draw_box(buf, area, Borders::ALL, &box_style());
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

    let left_area  = Rect::new(inner.x, inner.y, mid,              inner.height);
    let right_area = Rect::new(div_x,   inner.y, inner.width - mid, inner.height);
    let members    = member_list(app);

    render_user_pane(app, buf, left_area,  app.active_pane == Pane::Left);
    render_member_pane(app, buf, right_area, &members, app.active_pane == Pane::Right);
}

fn render_user_pane(app: &App, buf: &mut Buffer, area: Rect, active: bool) {
    if area.width < 10 { return; }
    let hs    = if active { s_head() } else { s_subhead() };
    let label = format!("all users ({})", app.users.len());
    ColumnGrid::write_text(buf, area, area.y, &label, Align::Start, hs);
    hline(buf, Rect::new(area.x, area.y + 1, area.width, 1));

    let cols = pair_grid().resolve(area);
    let data = Rect::new(area.x, area.y + 2, area.width, area.height.saturating_sub(2));
    let vis  = data.height as usize;

    for (i, user) in app.users.iter().enumerate().skip(app.left_offset).take(vis) {
        let y         = data.y + (i - app.left_offset) as u16;
        let sel       = active && i == app.left_cursor;
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

    for (i, user) in members.iter().enumerate().skip(app.right_offset).take(vis) {
        let y   = data.y + (i - app.right_offset) as u16;
        let sel = active && i == app.right_cursor;
        let sty = if sel { s_sel() } else if active { s_normal() } else { s_dim() };
        if sel { fill_row(buf, content.x, y, content.width, sty); }
        ColumnGrid::write_text(buf, cols[0], y, &user.uid, Align::Start, sty);
        ColumnGrid::write_text(buf, cols[2], y, &user.cn,  Align::Start, sty);
    }
}

// ─── column grids ────────────────────────────────────────────────────────────

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

fn pair_grid() -> ColumnGrid {
    ColumnGrid::new(vec![
        ColumnDef::fixed(12, ColumnKind::Text),              // uid
        ColumnDef::fixed(1,  ColumnKind::Custom),
        ColumnDef::fill(1,   ColumnKind::Text).with_min(8),  // name
    ])
}

// ─── drawing utilities ───────────────────────────────────────────────────────

fn inset(r: Rect, n: u16) -> Rect {
    Rect::new(r.x + n, r.y + n, r.width.saturating_sub(2 * n), r.height.saturating_sub(2 * n))
}

fn hline(buf: &mut Buffer, r: Rect) {
    for x in r.x..r.x + r.width {
        buf.set_string(x, r.y, "─", s_border());
    }
}

fn fill_row(buf: &mut Buffer, x: u16, y: u16, w: u16, style: Style) {
    for cx in x..x + w {
        buf.set_string(cx, y, " ", style);
    }
}
