//! TUI orchestrator: application state, event loop, key routing, render dispatch.

use std::time::Duration;

use crossterm::event::Event;
use mullion::{backend::CrosstermBackend, poll_event, Buffer, KeyCode, KeyModifiers, Rect, Terminal};

use crate::ldap::client::{Group, User};
use crate::session::Session;

use super::focus::{ListCursor, Pane};
use super::screens;

// ─── state ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode { Browse, GroupSelect, Membership }

pub struct App {
    sessions: Vec<Session>,
    active:   usize,
    mode:     Mode,

    pub users_cur:  ListCursor,
    pub groups_cur: ListCursor,

    // Browse screen.
    pub browse_focus: Pane,
    pub detail_scroll: usize,
    detail: Option<User>,        // full record of the cursored user (lazy-loaded)

    selected_group: usize,
    pub active_pane: Pane,
    pub left_cur:  ListCursor,
    pub right_cur: ListCursor,

    pub status:     Option<(String, bool)>,
    pub write_mode: bool,
}

impl App {
    fn new(sessions: Vec<Session>, write_mode: bool) -> Self {
        Self {
            sessions, active: 0, mode: Mode::Browse,
            users_cur: ListCursor::new(),
            groups_cur: ListCursor::new(),
            browse_focus: Pane::Left,
            detail_scroll: 0,
            detail: None,
            selected_group: 0,
            active_pane: Pane::Left,
            left_cur: ListCursor::new(),
            right_cur: ListCursor::new(),
            status: None, write_mode,
        }
    }

    // ── active session ──────────────────────────────────────────────────────

    fn session(&self) -> &Session { &self.sessions[self.active] }
    fn session_mut(&mut self) -> &mut Session { &mut self.sessions[self.active] }

    // ── accessors used by the screen renderers ──────────────────────────────

    pub fn users(&self) -> &[User] { &self.session().users }
    pub fn groups(&self) -> &[Group] { &self.session().groups }

    pub fn detail(&self) -> Option<&User> { self.detail.as_ref() }

    /// Names of the groups `uid` belongs to (active session).
    pub fn groups_of(&self, uid: &str) -> Vec<String> {
        self.groups().iter()
            .filter(|g| g.members.iter().any(|m| m == uid))
            .map(|g| g.name.clone())
            .collect()
    }

    pub fn selected_group(&self) -> Option<&Group> {
        self.groups().get(self.selected_group)
    }

    pub fn member_uids(&self) -> &[String] {
        self.selected_group().map(|g| g.members.as_slice()).unwrap_or(&[])
    }

    /// Members of the selected group, resolved to `User`s in `memberUid` order.
    pub fn member_list(&self) -> Vec<&User> {
        let users = self.users();
        self.member_uids().iter()
            .filter_map(|uid| users.iter().find(|u| &u.uid == uid))
            .collect()
    }

    /// The uid under the browse cursor, if any.
    fn cursor_uid(&self) -> Option<String> {
        self.users().get(self.users_cur.cursor).map(|u| u.uid.clone())
    }

    /// Fetch the full record for the cursored user if it isn't already loaded.
    fn ensure_detail_loaded(&mut self) {
        let Some(uid) = self.cursor_uid() else { self.detail = None; return; };
        if self.detail.as_ref().map(|u| u.uid.as_str()) == Some(uid.as_str()) {
            return;
        }
        match self.session_mut().client.get_user(&uid) {
            Ok(full) => { self.detail = full; self.detail_scroll = 0; }
            Err(e)   => { self.status = Some((format!("detail load failed: {e}"), true)); }
        }
    }
}

// ─── entry point ─────────────────────────────────────────────────────────────

pub fn run(sessions: Vec<Session>, write_mode: bool) -> anyhow::Result<()> {
    let mut app = App::new(sessions, write_mode);
    app.ensure_detail_loaded();

    let mut term = Terminal::new(CrosstermBackend::new(std::io::stdout()))?;
    term.enter()?;
    let result = main_loop(&mut term, &mut app);
    term.leave()?;

    // Unbind every session regardless of how the loop ended.
    for session in app.sessions {
        session.close().ok();
    }
    result
}

// ─── event loop ──────────────────────────────────────────────────────────────

fn main_loop(
    term: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    app:  &mut App,
) -> anyhow::Result<()> {
    loop {
        term.draw(|buf| {
            update_offsets(app, buf.area);
            render(app, buf);
        })?;

        match poll_event(Duration::from_millis(100))? {
            None => continue,
            Some(Event::Key(key)) => {
                if handle_key(app, key.code, key.modifiers)? {
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

    // Clamp detail-pane scroll so it can't run off the end of the content.
    // Body height = inner(area-2) - header(1) - sep(1) = vis - 2.
    let detail_vis = vis.saturating_sub(2);
    let max_scroll = screens::detail::row_count(app).saturating_sub(detail_vis.max(1));
    if app.detail_scroll > max_scroll { app.detail_scroll = max_scroll; }
}

// ─── key handling ────────────────────────────────────────────────────────────

fn handle_key(
    app:  &mut App,
    key:  KeyCode,
    mods: KeyModifiers,
) -> anyhow::Result<bool> {
    use KeyCode::*;

    if key == Char('c') && mods.contains(KeyModifiers::CONTROL) { return Ok(true); }

    app.status = None;

    match app.mode {
        Mode::Browse => match (app.browse_focus, key) {
            (_, Char('q')) | (_, Esc) => return Ok(true),
            (_, Char('g')) => { app.mode = Mode::GroupSelect; app.groups_cur.reset(); }
            (_, Tab) | (_, BackTab) => {
                app.browse_focus =
                    if app.browse_focus == Pane::Left { Pane::Right } else { Pane::Left };
            }
            // Left pane: navigate the user list (reloads the detail record).
            (Pane::Left, Up   | Char('k')) => { app.users_cur.up();              app.ensure_detail_loaded(); }
            (Pane::Left, Down | Char('j')) => { app.users_cur.down(app.users().len()); app.ensure_detail_loaded(); }
            (Pane::Left, PageUp)   => { app.users_cur.page(-10, app.users().len()); app.ensure_detail_loaded(); }
            (Pane::Left, PageDown) => { app.users_cur.page(10, app.users().len());  app.ensure_detail_loaded(); }
            // Right pane: scroll the detail body.
            (Pane::Right, Up   | Char('k')) => { app.detail_scroll = app.detail_scroll.saturating_sub(1); }
            (Pane::Right, Down | Char('j')) => { app.detail_scroll += 1; }
            (Pane::Right, PageUp)   => { app.detail_scroll = app.detail_scroll.saturating_sub(10); }
            (Pane::Right, PageDown) => { app.detail_scroll += 10; }
            _ => {}
        },

        Mode::GroupSelect => match key {
            Esc => { app.mode = Mode::Browse; }
            Up   | Char('k') => app.groups_cur.up(),
            Down | Char('j') => app.groups_cur.down(app.groups().len()),
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
                Pane::Left  => app.left_cur.down(app.users().len()),
                Pane::Right => app.right_cur.down(app.member_list().len()),
            },
            Enter => {
                if !app.write_mode {
                    app.status = Some(("Read-only — pass --write to modify".into(), true));
                } else {
                    do_membership_action(app)?;
                }
            }
            _ => {}
        },
    }
    Ok(false)
}

fn do_membership_action(app: &mut App) -> anyhow::Result<()> {
    let sel = app.selected_group;
    match app.active_pane {
        Pane::Left => {
            let uid  = app.users()[app.left_cur.cursor].uid.clone();
            let dn   = app.groups()[sel].dn.clone();
            let name = app.groups()[sel].name.clone();
            if app.member_uids().iter().any(|m| m == &uid) {
                app.status = Some((format!("{uid} is already in {name}"), false));
            } else {
                let session = app.session_mut();
                match session.client.group_add_member(&dn, &uid) {
                    Ok(()) => {
                        session.refresh_groups()?;
                        app.status = Some((format!("Added {uid} to {name}"), false));
                    }
                    Err(e) => { app.status = Some((format!("Error: {e}"), true)); }
                }
            }
        }
        Pane::Right => {
            let members = app.member_list();
            let uid = members.get(app.right_cur.cursor).map(|u| u.uid.clone());
            if let Some(uid) = uid {
                let dn   = app.groups()[sel].dn.clone();
                let name = app.groups()[sel].name.clone();
                let session = app.session_mut();
                match session.client.group_remove_member(&dn, &uid) {
                    Ok(()) => {
                        session.refresh_groups()?;
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
        Mode::Browse      => screens::users::render(app, buf, app.browse_focus),
        Mode::GroupSelect => screens::groups::render_select(app, buf),
        Mode::Membership  => screens::groups::render_membership(app, buf),
    }
}
