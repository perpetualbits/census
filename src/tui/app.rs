//! TUI orchestrator: application state, event loop, key routing, render dispatch.

use std::time::{Duration, Instant};

use crossterm::event::{Event, MouseEventKind};
use mullion::{backend::CrosstermBackend, Buffer, EventReader, KeyCode, KeyModifiers, Rect, Terminal};

use crate::ldap::client::{Group, User};
use crate::session::Session;

use super::focus::{ListCursor, Pane};
use super::glow;
use super::journal::{self, Journal};
use super::ldif;
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
    detail_photo: Option<mullion::video::Frame>, // decoded jpegPhoto for `detail`

    selected_group: usize,
    pub active_pane: Pane,
    pub left_cur:  ListCursor,
    pub right_cur: ListCursor,

    overlay: Option<Overlay>,
    pub status:     Option<(String, bool)>,
    pub write_mode: bool,
    pub dry_run:    bool,

    anim_start: Instant,
    anim_on: bool,

    /// Rollback journal (on-disk LDIF) + in-app undo stack.
    journal: Journal,
    /// Whether the bottom LDIF change-preview tile is shown.
    show_preview: bool,
}

impl App {
    fn new(sessions: Vec<Session>, write_mode: bool, dry_run: bool) -> Self {
        Self {
            sessions, active: 0, mode: Mode::Browse,
            users_cur: ListCursor::new(),
            groups_cur: ListCursor::new(),
            browse_focus: Pane::Left,
            detail_scroll: 0,
            detail_cur: 0,
            detail: None,
            detail_photo: None,
            selected_group: 0,
            active_pane: Pane::Left,
            left_cur: ListCursor::new(),
            right_cur: ListCursor::new(),
            overlay: None,
            status: None, write_mode, dry_run,
            anim_start: Instant::now(),
            anim_on: true,
            journal: Journal::new(),
            show_preview: false,
        }
    }

    /// May the write UI be opened? True in `--write` or `--dry-run`.
    fn can_write_ui(&self) -> bool { self.write_mode || self.dry_run }

    /// Short label of the current write mode, for the title bar.
    pub fn mode_tag(&self) -> &'static str {
        if self.dry_run { "dry-run" }
        else if self.write_mode { "write" }
        else { "read-only" }
    }

    // ── active session ──────────────────────────────────────────────────────

    fn session(&self) -> &Session { &self.sessions[self.active] }
    fn session_mut(&mut self) -> &mut Session { &mut self.sessions[self.active] }

    // ── accessors used by the screen renderers ──────────────────────────────

    pub fn users(&self) -> &[User] { &self.session().users }
    pub fn groups(&self) -> &[Group] { &self.session().groups }

    pub fn detail(&self) -> Option<&User> { self.detail.as_ref() }

    /// Connection/password facts for the active session (drives the top-border gap).
    pub fn conn_info(&self) -> &crate::conninfo::ConnInfo { &self.session().conn }

    /// Recent change records (LDIF blocks) for the preview tile.
    pub fn journal_log(&self) -> &[String] { &self.journal.log }

    /// Path of the on-disk rollback journal (shown in the preview footer).
    pub fn journal_path(&self) -> &std::path::Path { self.journal.path() }

    /// Count of undoable steps on the stack (shown in the preview header).
    pub fn undo_depth(&self) -> usize { self.journal.undo.len() }

    /// The decoded portrait for the cursored user, if it carries a `jpegPhoto`.
    pub fn detail_photo(&self) -> Option<&mullion::video::Frame> { self.detail_photo.as_ref() }

    /// Set the detail record and (re)decode its portrait in one place, so the cached
    /// photo never drifts from the user it belongs to.
    fn load_detail(&mut self, user: Option<User>) {
        self.detail_photo = user.as_ref()
            .and_then(|u| u.photo.as_deref())
            .and_then(super::photo::decode);
        self.detail = user;
    }

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
        let Some(uid) = self.cursor_uid() else { self.load_detail(None); return; };
        if self.detail.as_ref().map(|u| u.uid.as_str()) == Some(uid.as_str()) {
            return;
        }
        match self.session_mut().client.get_user(&uid) {
            Ok(full) => { self.load_detail(full); self.detail_scroll = 0; self.detail_cur = 0; }
            Err(e)   => { self.status = Some((format!("detail load failed: {e}"), true)); }
        }
    }
}

// ─── entry point ─────────────────────────────────────────────────────────────

pub fn run(sessions: Vec<Session>, write_mode: bool, dry_run: bool) -> anyhow::Result<()> {
    let mut app = App::new(sessions, write_mode, dry_run);
    app.ensure_detail_loaded();
    if dry_run {
        app.status = Some(("dry-run: writes are simulated, nothing is sent".into(), false));
    }

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
    // A background reader decouples input capture from the render cadence, so a
    // burst of keys is consumed in one frame and never blocked by a slow draw.
    let input = EventReader::new();
    loop {
        term.draw(|buf| {
            update_offsets(app, buf.area);
            render(app, buf);
        })?;

        // Cap the idle wait so the border glow keeps animating between events.
        let wait = if app.anim_on { RENDER_TICK } else { Duration::from_millis(100) };
        let Some(first) = input.recv_timeout(wait) else { continue };
        if dispatch(app, first)? {
            return Ok(());
        }
        // Drain the rest of any burst before the next redraw.
        for ev in input.drain() {
            if dispatch(app, ev)? {
                return Ok(());
            }
        }
    }
}

/// Route one captured event to the key/mouse handlers; returns `true` to quit.
fn dispatch(app: &mut App, ev: Event) -> anyhow::Result<bool> {
    match ev {
        Event::Key(key)  => handle_key(app, key.code, key.modifiers),
        Event::Mouse(me) => { handle_mouse(app, me.kind); Ok(false) }
        _ => Ok(false),
    }
}

/// Mouse handling: the wheel scrolls the active list/pane, mirroring `j`/`k`.
/// (Click-to-select is a later nicety — it needs per-frame pane geometry.)
fn handle_mouse(app: &mut App, kind: MouseEventKind) {
    let down = match kind {
        MouseEventKind::ScrollDown => true,
        MouseEventKind::ScrollUp   => false,
        _ => return,
    };
    // A modal overlay owns all input; ignore the wheel while one is open.
    if app.overlay.is_some() { return; }
    match app.mode {
        Mode::Browse => match app.browse_focus {
            Pane::Left => {
                if down { app.users_cur.down(app.users().len()); } else { app.users_cur.up(); }
                app.ensure_detail_loaded();
            }
            Pane::Right => {
                app.detail_scroll = if down {
                    app.detail_scroll.saturating_add(1)
                } else {
                    app.detail_scroll.saturating_sub(1)
                };
            }
        },
        Mode::GroupSelect => {
            if down { app.groups_cur.down(app.groups().len()); } else { app.groups_cur.up(); }
        }
        Mode::Membership => match app.active_pane {
            Pane::Left  => if down { app.left_cur.down(app.users().len()); } else { app.left_cur.up() },
            Pane::Right => if down { app.right_cur.down(app.member_list().len()); } else { app.right_cur.up() },
        },
    }
}

fn update_offsets(app: &mut App, area: Rect) {
    // border(2) + header(1) + sep(1) = 4
    let vis = area.height.saturating_sub(4) as usize;
    if vis == 0 { return; }

    let nusers = app.users().len();
    let ngroups = app.groups().len();
    app.users_cur.keep_in_view(nusers, vis);
    app.groups_cur.keep_in_view(ngroups, vis);
    app.left_cur.keep_in_view(nusers, vis);

    let rlen = app.member_uids().len();
    app.right_cur.clamp(rlen);
    app.right_cur.keep_in_view(rlen, vis);

    // Detail body height = inner(area-2) - header(1) - sep(1) = vis - 2, minus the
    // portrait band (photo rows + a gap) when the cursored user has a jpegPhoto.
    let prows = screens::detail::photo_rows(app, vis as u16) as usize;
    let photo_off = if prows > 0 { prows + 1 } else { 0 };
    let detail_vis = vis.saturating_sub(2).saturating_sub(photo_off).max(1);

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

    // `?` opens the manual from any screen.
    if key == Char('?') {
        app.overlay = Some(Overlay::Help(overlay::HelpView::new()));
        return Ok(false);
    }

    // `u` undoes the last reversible write from any screen (write mode only).
    if key == Char('u') && app.can_write_ui() {
        undo(app)?;
        return Ok(false);
    }

    // `L` toggles the LDIF change-preview tile.
    if key == Char('L') {
        app.show_preview = !app.show_preview;
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
                if !app.can_write_ui() {
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
    if !app.can_write_ui() {
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
    if !app.can_write_ui() {
        app.status = Some(("Read-only — pass --write to modify".into(), true));
        return;
    }
    let Some(user) = app.detail() else { return; };
    let editor = overlay::KeyEditor::new(user.dn.clone(), user.ssh_keys.clone());
    app.overlay = Some(Overlay::Keys(editor));
}

/// Open the set-password dialog for the cursored user.
fn open_passwd(app: &mut App) {
    if !app.can_write_ui() {
        app.status = Some(("Read-only — pass --write to modify".into(), true));
        return;
    }
    let Some(user) = app.detail() else { return; };
    let dlg = overlay::PasswdDialog::new(user.dn.clone(), user.uid.clone());
    app.overlay = Some(Overlay::Passwd(dlg));
}

/// Open the new-user form, seeded with the next free uidNumber.
fn open_new_user(app: &mut App) {
    if !app.can_write_ui() {
        app.status = Some(("Read-only — pass --write to modify".into(), true));
        return;
    }
    let suggested = app.session_mut().client.next_uid_number().unwrap_or(10000);
    app.overlay = Some(Overlay::NewUser(overlay::NewUserForm::new(suggested)));
}

/// Open a typed-DN delete confirmation for the user under the list cursor.
fn open_delete_user(app: &mut App) {
    if !app.can_write_ui() {
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
    if !app.can_write_ui() {
        app.status = Some(("Read-only — pass --write to modify".into(), true));
        return;
    }
    let suggested = app.session_mut().client.next_gid_number().unwrap_or(10000);
    app.overlay = Some(Overlay::NewGroup(overlay::NewGroupForm::new(suggested)));
}

/// Open a typed-DN delete confirmation for the group under the cursor.
fn open_delete_group(app: &mut App) {
    if !app.can_write_ui() {
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

/// One-line description of the LDAP operation an action would perform (dry-run).
fn describe_action(action: &Action) -> String {
    match action {
        Action::SetAttr { dn, attr, values } if values.is_empty() =>
            format!("DELETE attr {attr} on {dn}"),
        Action::SetAttr { dn, attr, values } =>
            format!("MODIFY {attr}={} on {dn}", values.join(",")),
        Action::SetKeys { dn, keys } =>
            format!("REPLACE sshPublicKey ({} key(s)) on {dn}", keys.len()),
        Action::AddMember { uid, group, .. } =>
            format!("ADD memberUid {uid} to {group}"),
        Action::DelMember { uid, group, .. } =>
            format!("DELETE memberUid {uid} from {group}"),
        Action::SetPasswd { dn, .. } =>
            format!("SET password on {dn}"),
        Action::CreateUser(s) =>
            format!("ADD user uid={} (uidNumber={}, gid={})", s.uid, s.uid_number, s.gid_number),
        Action::DeleteEntry { dn, .. } =>
            format!("DELETE {dn}"),
        Action::CreateGroup { name, gid_number } =>
            format!("ADD group cn={name} (gidNumber={gid_number})"),
        Action::DeleteGroup { dn, .. } =>
            format!("DELETE {dn}"),
        Action::RestoreEntry { dn, .. } =>
            format!("RESTORE {dn}"),
    }
}

/// Commit a requested [`Action`]: the single place writes happen. Gated on
/// `--write`, it captures the pre-state needed to reverse the change, applies it,
/// then journals the forward LDIF and pushes the inverse onto the undo stack.
fn perform(app: &mut App, action: Action) -> anyhow::Result<()> {
    // Dry-run: report the LDAP operation that would be sent, log the would-be LDIF
    // to the preview feed, change nothing.
    if app.dry_run {
        let base_dn = app.session().client.base_dn.clone();
        let schema  = app.session().client.schema().clone();
        let ldif    = ldif::action_ldif(&action, &base_dn, &schema);
        app.journal.note(&format!("[dry-run] {}", describe_action(&action)), &ldif);
        app.status = Some((format!("[dry-run] {}", describe_action(&action)), false));
        return Ok(());
    }
    if !app.can_write_ui() {
        app.status = Some(("Read-only — pass --write to modify".into(), true));
        return Ok(());
    }
    // Capture the inverse *before* applying — reversing a delete/replace needs the
    // old state, which is gone once the write lands.
    let inverse = inverse_of(app, &action);
    if apply(app, &action)? {
        record_action(app, &action);
        if let Some((label, inv)) = inverse {
            app.journal.undo.push(journal::UndoStep { label, inverse: inv });
        }
    }
    Ok(())
}

/// Undo the most recent reversible write by applying its stored inverse (which is
/// itself journaled, but pushes no new undo step).
fn undo(app: &mut App) -> anyhow::Result<()> {
    if app.dry_run {
        app.status = Some(("dry-run: nothing is actually written, so nothing to undo".into(), false));
        return Ok(());
    }
    if !app.can_write_ui() {
        app.status = Some(("Read-only — pass --write to modify".into(), true));
        return Ok(());
    }
    let Some(step) = app.journal.undo.pop() else {
        app.status = Some(("Nothing to undo".into(), false));
        return Ok(());
    };
    if apply(app, &step.inverse)? {
        record_action(app, &step.inverse);
        app.status = Some((format!("Undid: {}", step.label), false));
    }
    Ok(())
}

/// Execute one action against the active session's client, refresh the affected
/// caches, and set the status line. Returns `true` on success. Does **not** touch
/// the journal or undo stack — that is [`perform`]/[`undo`]'s job.
fn apply(app: &mut App, action: &Action) -> anyhow::Result<bool> {
    let ok = match action {
        Action::SetAttr { dn, attr, values } => {
            let refs: Vec<&str> = values.iter().map(String::as_str).collect();
            match app.session_mut().client.modify_replace(dn, attr, &refs) {
                Ok(()) => {
                    app.reload_detail_record();
                    let msg = if refs.is_empty() { format!("Cleared {attr}") } else { format!("Set {attr}") };
                    app.status = Some((msg, false));
                    true
                }
                Err(e) => { app.status = Some((format!("Error: {e}"), true)); false }
            }
        }
        Action::SetKeys { dn, keys } => {
            let n = keys.len();
            match app.session_mut().client.ssh_key_replace(dn, keys) {
                Ok(()) => { app.reload_detail_record(); app.status = Some((format!("Saved {n} ssh key(s)"), false)); true }
                Err(e) => { app.status = Some((format!("Error: {e}"), true)); false }
            }
        }
        Action::AddMember { group_dn, uid, group } => {
            match app.session_mut().client.group_add_member(group_dn, uid) {
                Ok(()) => { app.session_mut().refresh_groups()?; app.status = Some((format!("Added {uid} to {group}"), false)); true }
                Err(e) => { app.status = Some((format!("Error: {e}"), true)); false }
            }
        }
        Action::DelMember { group_dn, uid, group } => {
            match app.session_mut().client.group_remove_member(group_dn, uid) {
                Ok(()) => { app.session_mut().refresh_groups()?; app.status = Some((format!("Removed {uid} from {group}"), false)); true }
                Err(e) => { app.status = Some((format!("Error: {e}"), true)); false }
            }
        }
        Action::SetPasswd { dn, plaintext } => {
            match app.session_mut().client.set_password(dn, plaintext) {
                Ok(())  => { app.status = Some(("Password updated".into(), false)); true }
                Err(e)  => { app.status = Some((format!("Error: {e}"), true)); false }
            }
        }
        Action::CreateUser(spec) => {
            let uid = spec.uid.clone();
            match app.session_mut().client.add_user(spec) {
                Ok(_dn) => { app.session_mut().refresh_users()?; app.select_user(&uid); app.status = Some((format!("Created user {uid}"), false)); true }
                Err(e) => { app.status = Some((format!("Error: {e}"), true)); false }
            }
        }
        Action::DeleteEntry { dn, label } => {
            match app.session_mut().client.delete_entry(dn) {
                Ok(()) => { app.session_mut().refresh_users()?; app.clamp_and_reload_detail(); app.status = Some((format!("Deleted {label}"), false)); true }
                Err(e) => { app.status = Some((format!("Error: {e}"), true)); false }
            }
        }
        Action::CreateGroup { name, gid_number } => {
            match app.session_mut().client.add_group(name, *gid_number, &[]) {
                Ok(dn) => { app.session_mut().refresh_groups()?; app.select_group_by_dn(&dn); app.status = Some((format!("Created group {name}"), false)); true }
                Err(e) => { app.status = Some((format!("Error: {e}"), true)); false }
            }
        }
        Action::DeleteGroup { dn, name } => {
            match app.session_mut().client.delete_entry(dn) {
                Ok(()) => { app.session_mut().refresh_groups()?; app.groups_cur.clamp(app.groups().len()); app.status = Some((format!("Deleted group {name}"), false)); true }
                Err(e) => { app.status = Some((format!("Error: {e}"), true)); false }
            }
        }
        Action::RestoreEntry { dn, attrs, label } => {
            match app.session_mut().client.add_raw(dn, attrs) {
                Ok(()) => {
                    app.session_mut().refresh_users()?;
                    app.session_mut().refresh_groups()?;
                    app.status = Some((format!("Restored {label}"), false));
                    true
                }
                Err(e) => { app.status = Some((format!("Error: {e}"), true)); false }
            }
        }
    };
    Ok(ok)
}

/// Append the forward LDIF change record for `action` to the on-disk journal.
fn record_action(app: &mut App, action: &Action) {
    let base_dn = app.session().client.base_dn.clone();
    let schema  = app.session().client.schema().clone();
    let ldif    = ldif::action_ldif(action, &base_dn, &schema);
    let header  = format!("epoch {} | {} | {}", now_secs(), whoami(), describe_action(action));
    if let Err(e) = app.journal.record(&header, &ldif) {
        app.status = Some((format!("journal write failed: {e}"), true));
    }
}

/// Compute the inverse of `action` (and a human label), reading whatever pre-state
/// the reversal needs. `None` when the action cannot be undone (e.g. a password set,
/// whose previous value is unknowable).
fn inverse_of(app: &mut App, action: &Action) -> Option<(String, Action)> {
    match action {
        Action::AddMember { group_dn, uid, group } => Some((
            format!("add {uid} to {group}"),
            Action::DelMember { group_dn: group_dn.clone(), uid: uid.clone(), group: group.clone() },
        )),
        Action::DelMember { group_dn, uid, group } => Some((
            format!("remove {uid} from {group}"),
            Action::AddMember { group_dn: group_dn.clone(), uid: uid.clone(), group: group.clone() },
        )),
        Action::SetAttr { dn, attr, .. } => {
            let old = current_attr_values(app, dn, attr);
            Some((format!("edit {attr}"), Action::SetAttr { dn: dn.clone(), attr: attr.clone(), values: old }))
        }
        Action::SetKeys { dn, .. } => {
            let attr = app.session().client.schema().ssh_key;
            let old = current_attr_values(app, dn, attr);
            Some(("edit ssh keys".to_string(), Action::SetKeys { dn: dn.clone(), keys: old }))
        }
        Action::CreateUser(spec) => {
            let dn = app.session().client.schema().user_dn(&spec.uid, &app.session().client.base_dn);
            Some((format!("create user {}", spec.uid), Action::DeleteEntry { dn, label: spec.uid.clone() }))
        }
        Action::CreateGroup { name, .. } => {
            let c = &app.session().client;
            let dn = format!("{}={},{},{}", c.schema().cn, name, c.schema().group_ou, c.base_dn);
            Some((format!("create group {name}"), Action::DeleteGroup { dn, name: name.clone() }))
        }
        Action::DeleteEntry { dn, label } => {
            let attrs = app.session_mut().client.read_entry_raw(dn).ok()?;
            Some((format!("delete {label}"), Action::RestoreEntry { dn: dn.clone(), attrs, label: label.clone() }))
        }
        Action::DeleteGroup { dn, name } => {
            let attrs = app.session_mut().client.read_entry_raw(dn).ok()?;
            Some((format!("delete group {name}"), Action::RestoreEntry { dn: dn.clone(), attrs, label: name.clone() }))
        }
        // Restoring is itself reversible (delete again), so undo of a restore works.
        Action::RestoreEntry { dn, label, .. } =>
            Some((format!("restore {label}"), Action::DeleteEntry { dn: dn.clone(), label: label.clone() })),
        // A password set cannot be reversed: the old hash is not recoverable.
        Action::SetPasswd { .. } => None,
    }
}

/// Current values of `attr` on `dn` (empty when the attribute is absent), read
/// fresh so the captured inverse is exact.
fn current_attr_values(app: &mut App, dn: &str, attr: &str) -> Vec<String> {
    match app.session_mut().client.read_entry_raw(dn) {
        Ok(attrs) => attrs.into_iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(attr))
            .map(|(_, vals)| vals.iter().map(|v| String::from_utf8_lossy(v).into_owned()).collect())
            .unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn whoami() -> String {
    std::env::var("USER").unwrap_or_else(|_| "?".into())
}

impl App {
    /// Re-fetch the cursored user's full record and refresh the list cache so an
    /// edit is reflected in both the detail pane and the left list.
    fn reload_detail_record(&mut self) {
        if let Some(uid) = self.cursor_uid() {
            if let Ok(full) = self.session_mut().client.get_user(&uid) {
                self.load_detail(full);
            }
        }
        let _ = self.session_mut().refresh_users();
    }

    /// Move the list cursor to `uid` (if present) and load its detail.
    fn select_user(&mut self, uid: &str) {
        if let Some(i) = self.users().iter().position(|u| u.uid == uid) {
            self.users_cur.cursor = i;
        }
        self.load_detail(None);
        self.ensure_detail_loaded();
    }

    /// Clamp the list cursor to the (possibly shrunk) list and reload detail.
    fn clamp_and_reload_detail(&mut self) {
        let len = self.users().len();
        self.users_cur.clamp(len);
        self.load_detail(None);
        self.ensure_detail_loaded();
    }

    /// Move the group cursor to the group with DN `dn` (if present). Resolving by
    /// DN — not name — is what makes duplicate-named groups individually addressable.
    fn select_group_by_dn(&mut self, dn: &str) {
        if let Some(i) = self.groups().iter().position(|g| g.dn == dn) {
            self.groups_cur.cursor = i;
        }
    }
}

// ─── render dispatch ─────────────────────────────────────────────────────────

fn render(app: &App, buf: &mut Buffer) {
    // With the LDIF preview open, carve a bottom strip for it; the main screen
    // (and its glowing frame + connection gap) occupies the top.
    let full = buf.area;
    let (main, preview) = if app.show_preview && full.height > 10 {
        let ph = (full.height / 3).clamp(5, 14);
        let main = Rect::new(full.x, full.y, full.width, full.height - ph);
        let prev = Rect::new(full.x, full.y + full.height - ph, full.width, ph);
        (main, Some(prev))
    } else {
        (full, None)
    };

    match app.mode {
        Mode::Browse      => screens::users::render(app, buf, main, app.browse_focus),
        Mode::GroupSelect => screens::groups::render_select(app, buf, main),
        Mode::Membership  => screens::groups::render_membership(app, buf, main),
    }
    if let Some(pa) = preview {
        screens::preview::render(app, buf, pa);
    }

    // Connection/password "gap" in the top border of the main frame (content pass):
    // drawn over the frame the screen just laid down, and excluded from the glow.
    let gap = super::topgap::draw_top_gap(buf, main, &app.conn_info().summary());
    // Travelling glow on the main frame, under any modal overlay — skipping the gap.
    if app.anim_on {
        glow::edge_glow(buf, main, app.anim_start.elapsed().as_secs_f32(), gap.as_slice());
    }
    if let Some(ov) = &app.overlay {
        ov.render(buf, full);
    }
}
