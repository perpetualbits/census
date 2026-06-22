//! TUI orchestrator: application state, event loop, key routing, render dispatch.

use std::time::{Duration, Instant};

use crossterm::event::Event;
use mullion::{backend::CrosstermBackend, poll_event, Buffer, KeyCode, KeyModifiers, Rect, Terminal};

use crate::ldap::client::{Group, User};
use crate::session::Session;

use super::focus::{ListCursor, Pane};
use super::glow;
use super::overlay::{self, Action, Overlay, OverlayResult};
use super::screens;

/// Idle redraw cap — keeps the border glow animating at ~20 fps.
const RENDER_TICK: Duration = Duration::from_millis(50);

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
    pub detail_cur: usize,       // index into the detail pane's editable targets
    detail: Option<User>,        // full record of the cursored user (lazy-loaded)

    selected_group: usize,
    pub active_pane: Pane,
    pub left_cur:  ListCursor,
    pub right_cur: ListCursor,

    overlay: Option<Overlay>,
    pub status:     Option<(String, bool)>,
    pub write_mode: bool,

    anim_start: Instant,
    anim_on: bool,
}

impl App {
    fn new(sessions: Vec<Session>, write_mode: bool) -> Self {
        Self {
            sessions, active: 0, mode: Mode::Browse,
            users_cur: ListCursor::new(),
            groups_cur: ListCursor::new(),
            browse_focus: Pane::Left,
            detail_scroll: 0,
            detail_cur: 0,
            detail: None,
            selected_group: 0,
            active_pane: Pane::Left,
            left_cur: ListCursor::new(),
            right_cur: ListCursor::new(),
            overlay: None,
            status: None, write_mode,
            anim_start: Instant::now(),
            anim_on: true,
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
            Ok(full) => { self.detail = full; self.detail_scroll = 0; self.detail_cur = 0; }
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

        // Cap the wait so the border glow keeps animating while idle.
        let wait = if app.anim_on { RENDER_TICK } else { Duration::from_millis(100) };
        match poll_event(wait)? {
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

    // Detail body height = inner(area-2) - header(1) - sep(1) = vis - 2.
    let detail_vis = vis.saturating_sub(2).max(1);

    // Keep the selected editable attribute (right pane) within the body.
    if app.mode == Mode::Browse && app.browse_focus == Pane::Right {
        let ntargets = screens::detail::edit_targets(app).len();
        if ntargets > 0 && app.detail_cur >= ntargets { app.detail_cur = ntargets - 1; }
        if let Some(row) = screens::detail::target_row(app, app.detail_cur) {
            if row < app.detail_scroll {
                app.detail_scroll = row;
            } else if row >= app.detail_scroll + detail_vis {
                app.detail_scroll = row + 1 - detail_vis;
            }
        }
    }

    // Clamp detail-pane scroll so it can't run off the end of the content.
    let max_scroll = screens::detail::row_count(app).saturating_sub(detail_vis);
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

    // Ctrl-G toggles the border glow (motion off switch).
    if key == Char('g') && mods.contains(KeyModifiers::CONTROL) {
        app.anim_on = !app.anim_on;
        return Ok(false);
    }

    // A modal overlay, when present, consumes every key.
    if let Some(ov) = &mut app.overlay {
        match ov.handle_key(key, mods) {
            OverlayResult::Stay   => {}
            OverlayResult::Cancel => app.overlay = None,
            OverlayResult::Commit(action) => {
                app.overlay = None;
                perform(app, action)?;
            }
        }
        return Ok(false);
    }

    app.status = None;

    match app.mode {
        Mode::Browse => match (app.browse_focus, key) {
            (_, Char('q')) => return Ok(true),
            // Esc steps focus back to the list, then quits.
            (Pane::Right, Esc) => app.browse_focus = Pane::Left,
            (Pane::Left,  Esc) => return Ok(true),
            (_, Char('g')) => { app.mode = Mode::GroupSelect; app.groups_cur.reset(); }
            (_, Char('n')) => open_new_user(app),
            (_, Char('D')) => open_delete_user(app),
            (_, Tab) | (_, BackTab) => {
                app.browse_focus =
                    if app.browse_focus == Pane::Left { Pane::Right } else { Pane::Left };
            }
            // Left pane: navigate the user list (reloads the detail record).
            (Pane::Left, Up   | Char('k')) => { app.users_cur.up();              app.ensure_detail_loaded(); }
            (Pane::Left, Down | Char('j')) => { app.users_cur.down(app.users().len()); app.ensure_detail_loaded(); }
            (Pane::Left, PageUp)   => { app.users_cur.page(-10, app.users().len()); app.ensure_detail_loaded(); }
            (Pane::Left, PageDown) => { app.users_cur.page(10, app.users().len());  app.ensure_detail_loaded(); }
            // Right pane: move the editable-attribute cursor.
            (Pane::Right, Up   | Char('k')) => { app.detail_cur = app.detail_cur.saturating_sub(1); }
            (Pane::Right, Down | Char('j')) => {
                let n = screens::detail::edit_targets(app).len();
                if app.detail_cur + 1 < n { app.detail_cur += 1; }
            }
            (Pane::Right, Char('e')) => open_attr_edit(app),
            (Pane::Right, Char('K')) => open_key_editor(app),
            (Pane::Right, Char('p')) => open_passwd(app),
            _ => {}
        },

        Mode::GroupSelect => match key {
            Esc => { app.mode = Mode::Browse; }
            Up   | Char('k') => app.groups_cur.up(),
            Down | Char('j') => app.groups_cur.down(app.groups().len()),
            Char('n') => open_new_group(app),
            Char('D') => open_delete_group(app),
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
    let sel   = app.selected_group;
    let group = app.groups()[sel].name.clone();
    let dn    = app.groups()[sel].dn.clone();
    match app.active_pane {
        Pane::Left => {
            // Adding a member: no confirmation needed.
            let uid = app.users()[app.left_cur.cursor].uid.clone();
            if app.member_uids().iter().any(|m| m == &uid) {
                app.status = Some((format!("{uid} is already in {group}"), false));
            } else {
                perform(app, Action::AddMember { group_dn: dn, uid, group })?;
            }
        }
        Pane::Right => {
            // Removing a member: confirm first.
            let members = app.member_list();
            if let Some(uid) = members.get(app.right_cur.cursor).map(|u| u.uid.clone()) {
                let prompt = format!("Remove {uid} from {group}?");
                let action = Action::DelMember { group_dn: dn, uid, group };
                app.overlay = Some(Overlay::Confirm(overlay::ConfirmDialog::yes_no(prompt, action)));
            }
        }
    }
    Ok(())
}

/// Open the attribute-edit modal for the detail pane's selected target.
fn open_attr_edit(app: &mut App) {
    if !app.write_mode {
        app.status = Some(("Read-only — pass --write to modify".into(), true));
        return;
    }
    let targets = screens::detail::edit_targets(app);
    let Some(target) = targets.get(app.detail_cur) else { return; };
    let dn = match app.detail() { Some(u) => u.dn.clone(), None => return };
    let dlg = overlay::InputDialog::edit_attr(dn, target.attr.clone(), &target.value);
    app.overlay = Some(Overlay::Input(dlg));
}

/// Open the SSH-key manager for the cursored user.
fn open_key_editor(app: &mut App) {
    if !app.write_mode {
        app.status = Some(("Read-only — pass --write to modify".into(), true));
        return;
    }
    let Some(user) = app.detail() else { return; };
    let editor = overlay::KeyEditor::new(user.dn.clone(), user.ssh_keys.clone());
    app.overlay = Some(Overlay::Keys(editor));
}

/// Open the set-password dialog for the cursored user.
fn open_passwd(app: &mut App) {
    if !app.write_mode {
        app.status = Some(("Read-only — pass --write to modify".into(), true));
        return;
    }
    let Some(user) = app.detail() else { return; };
    let dlg = overlay::PasswdDialog::new(user.dn.clone(), user.uid.clone());
    app.overlay = Some(Overlay::Passwd(dlg));
}

/// Open the new-user form, seeded with the next free uidNumber.
fn open_new_user(app: &mut App) {
    if !app.write_mode {
        app.status = Some(("Read-only — pass --write to modify".into(), true));
        return;
    }
    let suggested = app.session_mut().client.next_uid_number().unwrap_or(10000);
    app.overlay = Some(Overlay::NewUser(overlay::NewUserForm::new(suggested)));
}

/// Open a typed-DN delete confirmation for the user under the list cursor.
fn open_delete_user(app: &mut App) {
    if !app.write_mode {
        app.status = Some(("Read-only — pass --write to modify".into(), true));
        return;
    }
    let Some(user) = app.users().get(app.users_cur.cursor) else { return; };
    let dn  = user.dn.clone();
    let uid = user.uid.clone();
    let prompt = format!("Delete user {uid}? Irreversible.");
    let action = Action::DeleteEntry { dn: dn.clone(), label: uid };
    app.overlay = Some(Overlay::Confirm(overlay::ConfirmDialog::typed_dn(prompt, dn, action)));
}

/// Open the new-group form, seeded with the next free gidNumber.
fn open_new_group(app: &mut App) {
    if !app.write_mode {
        app.status = Some(("Read-only — pass --write to modify".into(), true));
        return;
    }
    let suggested = app.session_mut().client.next_gid_number().unwrap_or(10000);
    app.overlay = Some(Overlay::NewGroup(overlay::NewGroupForm::new(suggested)));
}

/// Open a typed-DN delete confirmation for the group under the cursor.
fn open_delete_group(app: &mut App) {
    if !app.write_mode {
        app.status = Some(("Read-only — pass --write to modify".into(), true));
        return;
    }
    let Some(group) = app.groups().get(app.groups_cur.cursor) else { return; };
    let dn   = group.dn.clone();
    let name = group.name.clone();
    let prompt = format!("Delete group {name}? Irreversible.");
    let action = Action::DeleteGroup { dn: dn.clone(), name };
    app.overlay = Some(Overlay::Confirm(overlay::ConfirmDialog::typed_dn(prompt, dn, action)));
}

// ─── write chokepoint ─────────────────────────────────────────────────────────

/// Execute a committed [`Action`]. The single place writes happen: gated on
/// `--write`, then dispatched to the active session's client, then the affected
/// caches are refreshed.
fn perform(app: &mut App, action: Action) -> anyhow::Result<()> {
    if !app.write_mode {
        app.status = Some(("Read-only — pass --write to modify".into(), true));
        return Ok(());
    }
    match action {
        Action::SetAttr { dn, attr, values } => {
            let refs: Vec<&str> = values.iter().map(String::as_str).collect();
            match app.session_mut().client.modify_replace(&dn, &attr, &refs) {
                Ok(()) => {
                    app.reload_detail_record();
                    let msg = if refs.is_empty() {
                        format!("Cleared {attr}")
                    } else {
                        format!("Set {attr}")
                    };
                    app.status = Some((msg, false));
                }
                Err(e) => { app.status = Some((format!("Error: {e}"), true)); }
            }
        }
        Action::SetKeys { dn, keys } => {
            let n = keys.len();
            match app.session_mut().client.ssh_key_replace(&dn, &keys) {
                Ok(()) => {
                    app.reload_detail_record();
                    app.status = Some((format!("Saved {n} ssh key(s)"), false));
                }
                Err(e) => { app.status = Some((format!("Error: {e}"), true)); }
            }
        }
        Action::AddMember { group_dn, uid, group } => {
            let session = app.session_mut();
            match session.client.group_add_member(&group_dn, &uid) {
                Ok(()) => {
                    session.refresh_groups()?;
                    app.status = Some((format!("Added {uid} to {group}"), false));
                }
                Err(e) => { app.status = Some((format!("Error: {e}"), true)); }
            }
        }
        Action::DelMember { group_dn, uid, group } => {
            let session = app.session_mut();
            match session.client.group_remove_member(&group_dn, &uid) {
                Ok(()) => {
                    session.refresh_groups()?;
                    app.status = Some((format!("Removed {uid} from {group}"), false));
                }
                Err(e) => { app.status = Some((format!("Error: {e}"), true)); }
            }
        }
        Action::SetPasswd { dn, plaintext } => {
            match app.session_mut().client.set_password(&dn, &plaintext) {
                Ok(())  => { app.status = Some(("Password updated".into(), false)); }
                Err(e)  => { app.status = Some((format!("Error: {e}"), true)); }
            }
        }
        Action::CreateUser(spec) => {
            let uid = spec.uid.clone();
            match app.session_mut().client.add_user(&spec) {
                Ok(_dn) => {
                    app.session_mut().refresh_users()?;
                    app.select_user(&uid);
                    app.status = Some((format!("Created user {uid}"), false));
                }
                Err(e) => { app.status = Some((format!("Error: {e}"), true)); }
            }
        }
        Action::DeleteEntry { dn, label } => {
            match app.session_mut().client.delete_entry(&dn) {
                Ok(()) => {
                    app.session_mut().refresh_users()?;
                    app.clamp_and_reload_detail();
                    app.status = Some((format!("Deleted {label}"), false));
                }
                Err(e) => { app.status = Some((format!("Error: {e}"), true)); }
            }
        }
        Action::CreateGroup { name, gid_number } => {
            match app.session_mut().client.add_group(&name, gid_number, &[]) {
                Ok(_dn) => {
                    app.session_mut().refresh_groups()?;
                    app.select_group(&name);
                    app.status = Some((format!("Created group {name}"), false));
                }
                Err(e) => { app.status = Some((format!("Error: {e}"), true)); }
            }
        }
        Action::DeleteGroup { dn, name } => {
            match app.session_mut().client.delete_entry(&dn) {
                Ok(()) => {
                    app.session_mut().refresh_groups()?;
                    app.groups_cur.clamp(app.groups().len());
                    app.status = Some((format!("Deleted group {name}"), false));
                }
                Err(e) => { app.status = Some((format!("Error: {e}"), true)); }
            }
        }
    }
    Ok(())
}

impl App {
    /// Re-fetch the cursored user's full record and refresh the list cache so an
    /// edit is reflected in both the detail pane and the left list.
    fn reload_detail_record(&mut self) {
        if let Some(uid) = self.cursor_uid() {
            if let Ok(full) = self.session_mut().client.get_user(&uid) {
                self.detail = full;
            }
        }
        let _ = self.session_mut().refresh_users();
    }

    /// Move the list cursor to `uid` (if present) and load its detail.
    fn select_user(&mut self, uid: &str) {
        if let Some(i) = self.users().iter().position(|u| u.uid == uid) {
            self.users_cur.cursor = i;
        }
        self.detail = None;
        self.ensure_detail_loaded();
    }

    /// Clamp the list cursor to the (possibly shrunk) list and reload detail.
    fn clamp_and_reload_detail(&mut self) {
        let len = self.users().len();
        self.users_cur.clamp(len);
        self.detail = None;
        self.ensure_detail_loaded();
    }

    /// Move the group cursor to the group named `name` (if present).
    fn select_group(&mut self, name: &str) {
        if let Some(i) = self.groups().iter().position(|g| g.name == name) {
            self.groups_cur.cursor = i;
        }
    }
}

// ─── render dispatch ─────────────────────────────────────────────────────────

fn render(app: &App, buf: &mut Buffer) {
    match app.mode {
        Mode::Browse      => screens::users::render(app, buf, app.browse_focus),
        Mode::GroupSelect => screens::groups::render_select(app, buf),
        Mode::Membership  => screens::groups::render_membership(app, buf),
    }
    // Travelling glow on the outer frame, under any modal overlay.
    if app.anim_on {
        glow::edge_glow(buf, buf.area, app.anim_start.elapsed().as_secs_f32());
    }
    if let Some(ov) = &app.overlay {
        ov.render(buf, buf.area);
    }
}
