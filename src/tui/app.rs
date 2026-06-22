//! TUI orchestrator: application state, event loop, key routing, render dispatch.

use std::time::Duration;

use crossterm::event::Event;
use mullion::{backend::CrosstermBackend, poll_event, Buffer, KeyCode, KeyModifiers, Rect, Terminal};

use crate::config::Config;
use crate::ldap::client::{Group, LdapClient, User};

use super::focus::{ListCursor, Pane};
use super::screens;

// ─── state ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode { Browse, GroupSelect, Membership }

pub struct App {
    users:  Vec<User>,
    groups: Vec<Group>,
    mode:   Mode,

    pub users_cur:  ListCursor,
    pub groups_cur: ListCursor,

    selected_group: usize,
    pub active_pane: Pane,
    pub left_cur:  ListCursor,
    pub right_cur: ListCursor,

    pub status:     Option<(String, bool)>,
    pub write_mode: bool,

    // precomputed: user index → group names that user belongs to
    user_groups: Vec<Vec<String>>,
}

impl App {
    fn new(users: Vec<User>, groups: Vec<Group>, write_mode: bool) -> Self {
        let user_groups = build_user_groups(&users, &groups);
        Self {
            users, groups, mode: Mode::Browse,
            users_cur: ListCursor::new(),
            groups_cur: ListCursor::new(),
            selected_group: 0,
            active_pane: Pane::Left,
            left_cur: ListCursor::new(),
            right_cur: ListCursor::new(),
            status: None, write_mode, user_groups,
        }
    }

    // ── accessors used by the screen renderers ──────────────────────────────

    pub fn users(&self) -> &[User] { &self.users }
    pub fn groups(&self) -> &[Group] { &self.groups }
    pub fn user_groups(&self) -> &[Vec<String>] { &self.user_groups }

    pub fn selected_group(&self) -> Option<&Group> {
        self.groups.get(self.selected_group)
    }

    pub fn member_uids(&self) -> &[String] {
        self.selected_group().map(|g| g.members.as_slice()).unwrap_or(&[])
    }

    /// Members of the selected group, resolved to `User`s in `memberUid` order.
    pub fn member_list(&self) -> Vec<&User> {
        self.member_uids().iter()
            .filter_map(|uid| self.users.iter().find(|u| &u.uid == uid))
            .collect()
    }

    // ── refresh ─────────────────────────────────────────────────────────────

    fn refresh_groups(&mut self, client: &mut LdapClient) -> anyhow::Result<()> {
        self.groups = client.list_groups()?;
        self.user_groups = build_user_groups(&self.users, &self.groups);
        Ok(())
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

fn update_offsets(app: &mut App, area: Rect) {
    // border(2) + header(1) + sep(1) = 4
    let vis = area.height.saturating_sub(4) as usize;
    if vis == 0 { return; }

    app.users_cur.keep_in_view(vis);
    app.groups_cur.keep_in_view(vis);
    app.left_cur.keep_in_view(vis);

    let rlen = app.member_uids().len();
    app.right_cur.clamp(rlen);
    app.right_cur.keep_in_view(vis);
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
            Char('g') => { app.mode = Mode::GroupSelect; app.groups_cur.reset(); }
            Up   | Char('k') => app.users_cur.up(),
            Down | Char('j') => app.users_cur.down(app.users.len()),
            PageUp   => app.users_cur.page(-10, app.users.len()),
            PageDown => app.users_cur.page(10, app.users.len()),
            _ => {}
        },

        Mode::GroupSelect => match key {
            Esc => { app.mode = Mode::Browse; }
            Up   | Char('k') => app.groups_cur.up(),
            Down | Char('j') => app.groups_cur.down(app.groups.len()),
            Enter => {
                app.selected_group = app.groups_cur.cursor;
                app.mode = Mode::Membership;
                app.active_pane = Pane::Left;
                app.left_cur.reset();
                app.right_cur.reset();
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
                Pane::Left  => app.left_cur.up(),
                Pane::Right => app.right_cur.up(),
            },
            Down | Char('j') => match app.active_pane {
                Pane::Left  => app.left_cur.down(app.users.len()),
                Pane::Right => app.right_cur.down(app.member_list().len()),
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
            let uid  = app.users[app.left_cur.cursor].uid.clone();
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
            let members = app.member_list();
            if let Some(user) = members.get(app.right_cur.cursor) {
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
        Mode::Browse      => screens::users::render(app, buf),
        Mode::GroupSelect => screens::groups::render_select(app, buf),
        Mode::Membership  => screens::groups::render_membership(app, buf),
    }
}
