//! TUI orchestrator: application state, event loop, key routing, render dispatch.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use crossterm::event::{Event, MouseEventKind};
use mullion::{
    backend::CrosstermBackend, Buffer, EventReader, KeyCode, KeyModifiers, Rect, Terminal,
};

use crate::config::Config;
use crate::ldap::client::{DitNode, Group, User};
use crate::session::Session;

use super::browse::Browse;

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
enum Mode { Browse, GroupSelect, Membership, Dit, Search }

/// What a [`SearchHit`] points at, so `Enter` can jump to the right screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum HitKind { User, Group }

/// One row in the cross-directory search results: a user or a group that matched
/// the query, with the display text and the key (`uid` / group `dn`) used to jump.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SearchHit {
    pub kind: HitKind,
    pub key: String,        // uid (user) or dn (group)
    pub primary: String,    // main label: uid or group name
    pub secondary: String,  // dim context: cn · uid/gid, or gid · aliases
    pub rank: u8,           // 0 exact, 1 prefix, 2 substring (lower sorts first)
}

/// One flattened row of the DIT tree browser (built from the children cache +
/// expand-set each frame).
pub struct DitRow {
    pub dn: String,
    pub label: String,
    /// For each ancestor level, whether that ancestor was its parent's last child
    /// (drives `│` vs blank guide columns in [`mullion::render_tree_row`]).
    pub ancestor_last: Vec<bool>,
    pub is_last: bool,
    /// `Some(false)` collapsed, `Some(true)` expanded, `None` leaf (fetched, no kids).
    pub expanded: Option<bool>,
}

pub struct App {
    sessions: Vec<Session>,
    active:   usize,
    mode:     Mode,

    pub groups_cur: ListCursor,

    // Browse screen — the windowed user list runs on a background thread (browse.rs)
    // so a slow server fetch never blocks the UI; here we hold the latest snapshot.
    browse: Browse,
    // The attribute the browse is sorted/keyed by (`[browse] sort_attr`, default `uid`),
    // used to turn a search hit's uid into its VLV paging key.
    browse_sort_attr: String,

    // Browse screen.
    pub browse_focus: Pane,
    pub detail_scroll: usize,
    pub detail_cur: usize,       // index into the detail pane's editable targets
    detail: Option<User>,        // full record of the cursored user (lazy-loaded)
    detail_photo: Option<mullion::video::Frame>, // decoded jpegPhoto for `detail`

    // Group browse screen (list + group detail), mirroring the user browse screen.
    pub group_browse_focus: Pane,
    pub group_detail_cur: usize,
    pub group_detail_scroll: usize,

    // DIT tree browser.
    pub dit_focus: Pane,
    pub dit_cur: ListCursor,
    pub dit_detail_cur: usize,
    dit_children: HashMap<String, Vec<DitNode>>, // parent DN → its children (lazy cache)
    dit_truncated: HashSet<String>,              // DNs whose child fetch hit the size cap
    dit_expanded: HashSet<String>,               // DNs currently expanded
    dit_rows: Vec<DitRow>,                        // flattened visible rows (rebuilt on change)
    dit_detail: Vec<(String, Vec<String>)>,      // selected entry's attributes

    // Cross-directory search (opened with `/` from any screen).
    pub search_cur: ListCursor,          // selection within the results list
    search_query: String,                // the live query text
    search_caret: usize,                 // caret (byte offset) within the query field
    search_hits: Vec<SearchHit>,         // matches from the last server query
    search_from: Mode,                   // screen to return to on Esc
    search_dirty: bool,                  // query edited; a debounced server search is pending
    search_last_edit: Instant,           // when the query last changed (drives the debounce)

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
    fn new(sessions: Vec<Session>, write_mode: bool, dry_run: bool, cfg: Config, password: Option<String>) -> Self {
        // The attribute the browse list is sorted/keyed by; needed to resolve a
        // search-jump's paging key (captured before `cfg` moves into the worker).
        let browse_sort_attr = cfg.browse.sort_attr.clone();
        // The browse list runs on its own thread with its own read-only connection.
        let browse = Browse::spawn(cfg, password, 20);
        Self {
            sessions, active: 0, mode: Mode::Browse,
            browse, browse_sort_attr,
            groups_cur: ListCursor::new(),
            browse_focus: Pane::Left,
            detail_scroll: 0,
            detail_cur: 0,
            detail: None,
            detail_photo: None,
            group_browse_focus: Pane::Left,
            group_detail_cur: 0,
            group_detail_scroll: 0,
            dit_focus: Pane::Left,
            dit_cur: ListCursor::new(),
            dit_detail_cur: 0,
            dit_children: HashMap::new(),
            dit_truncated: HashSet::new(),
            dit_expanded: HashSet::new(),
            dit_rows: Vec::new(),
            dit_detail: Vec::new(),
            search_cur: ListCursor::new(),
            search_query: String::new(),
            search_caret: 0,
            search_hits: Vec::new(),
            search_from: Mode::Browse,
            search_dirty: false,
            search_last_edit: Instant::now(),
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

    /// Whether the user/group list was capped at the browse size limit (more exist).
    #[allow(dead_code)] // the browse user list is now virtualized; groups still uses this
    pub fn users_truncated(&self) -> bool { self.session().users_truncated }
    pub fn groups_truncated(&self) -> bool { self.session().groups_truncated }
    /// Whether the DIT root's children were capped (drives a header marker).
    pub fn dit_root_truncated(&self) -> bool { self.dit_truncated.contains(&self.dit_base()) }

    pub fn detail(&self) -> Option<&User> { self.detail.as_ref() }

    /// Connection/password facts for the active session (drives the top-border gap).
    pub fn conn_info(&self) -> &crate::conninfo::ConnInfo { &self.session().conn }

    /// Recent change records (LDIF blocks) for the preview tile.
    pub fn journal_log(&self) -> &[String] { &self.journal.log }

    /// Path of the on-disk rollback journal (shown in the preview footer).
    pub fn journal_path(&self) -> &std::path::Path { self.journal.path() }

    /// Count of undoable steps on the stack (shown in the preview header).
    pub fn undo_depth(&self) -> usize { self.journal.undo.len() }

    // ── DIT tree browser ─────────────────────────────────────────────────────

    pub fn dit_rows(&self) -> &[DitRow] { &self.dit_rows }
    pub fn dit_detail(&self) -> &[(String, Vec<String>)] { &self.dit_detail }
    pub fn dit_base(&self) -> String { self.session().client.base_dn.clone() }

    /// Enter the browser: fetch the base DN's children as the top level.
    fn enter_dit(&mut self) {
        self.dit_focus = Pane::Left;
        self.dit_cur.reset();
        self.dit_detail_cur = 0;
        self.dit_children.clear();
        self.dit_truncated.clear();
        self.dit_expanded.clear();
        let base = self.dit_base();
        if let Ok((kids, trunc)) = self.session_mut().client.list_children(&base) {
            if trunc { self.dit_truncated.insert(base.clone()); }
            self.dit_children.insert(base, kids);
        }
        self.rebuild_dit_rows();
        self.load_dit_detail();
    }

    /// Rebuild the flattened visible rows from the children cache + expand-set.
    fn rebuild_dit_rows(&mut self) {
        let base = self.dit_base();
        let mut rows = Vec::new();
        if let Some(top) = self.dit_children.get(&base).cloned() {
            let n = top.len();
            for (i, node) in top.iter().enumerate() {
                self.walk_dit(node, &[], i + 1 == n, &mut rows);
            }
        }
        self.dit_rows = rows;
    }

    fn walk_dit(&self, node: &DitNode, ancestor_last: &[bool], is_last: bool, rows: &mut Vec<DitRow>) {
        let fetched = self.dit_children.contains_key(&node.dn);
        let kids = self.dit_children.get(&node.dn);
        let expanded = if fetched && kids.is_none_or(|c| c.is_empty()) {
            None // fetched with no children → leaf
        } else if self.dit_expanded.contains(&node.dn) {
            Some(true)
        } else {
            Some(false)
        };
        // A capped node has more children than were fetched — flag it so the operator
        // isn't misled into thinking the shown children are all of them.
        let mut label = node.rdn.clone();
        if self.dit_truncated.contains(&node.dn) {
            label.push_str("  ⋯ capped");
        }
        rows.push(DitRow {
            dn: node.dn.clone(),
            label,
            ancestor_last: ancestor_last.to_vec(),
            is_last,
            expanded,
        });
        if self.dit_expanded.contains(&node.dn) {
            if let Some(children) = kids {
                let m = children.len();
                let mut al = ancestor_last.to_vec();
                al.push(is_last);
                for (i, kid) in children.iter().enumerate() {
                    self.walk_dit(kid, &al, i + 1 == m, rows);
                }
            }
        }
    }

    fn dit_selected_dn(&self) -> Option<String> {
        self.dit_rows.get(self.dit_cur.cursor).map(|r| r.dn.clone())
    }

    /// Load the selected entry's attributes into the detail pane.
    fn load_dit_detail(&mut self) {
        self.dit_detail_cur = 0;
        match self.dit_selected_dn() {
            Some(dn) => {
                self.dit_detail = self.session_mut().client.read_entry_display(&dn).unwrap_or_default();
            }
            None => self.dit_detail.clear(),
        }
    }

    /// Expand (fetching children on first open) or collapse the selected node.
    fn dit_toggle(&mut self) {
        let Some(dn) = self.dit_selected_dn() else { return; };
        if self.dit_expanded.contains(&dn) {
            self.dit_expanded.remove(&dn);
        } else {
            if !self.dit_children.contains_key(&dn) {
                match self.session_mut().client.list_children(&dn) {
                    Ok((kids, trunc)) => {
                        if trunc { self.dit_truncated.insert(dn.clone()); }
                        self.dit_children.insert(dn.clone(), kids);
                    }
                    Err(e) => { self.status = Some((format!("expand failed: {e}"), true)); return; }
                }
            }
            if self.dit_children.get(&dn).is_some_and(|c| !c.is_empty()) {
                self.dit_expanded.insert(dn);
            }
        }
        self.rebuild_dit_rows();
        self.dit_cur.clamp(self.dit_rows.len());
    }

    fn dit_collapse(&mut self) {
        if let Some(dn) = self.dit_selected_dn() {
            if self.dit_expanded.remove(&dn) {
                self.rebuild_dit_rows();
                self.dit_cur.clamp(self.dit_rows.len());
            }
        }
    }

    /// Re-fetch expanded nodes after a write and rebuild the tree + detail.
    fn refresh_dit(&mut self) {
        let base = self.dit_base();
        let mut targets: Vec<String> = vec![base];
        targets.extend(self.dit_expanded.iter().cloned());
        self.dit_children.clear();
        self.dit_truncated.clear();
        for dn in targets {
            if let Ok((kids, trunc)) = self.session_mut().client.list_children(&dn) {
                if trunc { self.dit_truncated.insert(dn.clone()); }
                self.dit_children.insert(dn, kids);
            }
        }
        self.rebuild_dit_rows();
        self.dit_cur.clamp(self.dit_rows.len());
        self.load_dit_detail();
    }

    /// Editable attributes of the DIT-selected entry (single-valued, admin-safe).
    pub fn dit_edit_targets(&self) -> Vec<(String, String)> {
        const SKIP: &[&str] = &["objectClass", "userPassword", "structuralObjectClass",
                                "entryUUID", "entryCSN", "creatorsName", "modifiersName"];
        self.dit_detail.iter()
            .filter(|(k, v)| v.len() == 1 && !SKIP.contains(&k.as_str())
                             && !v[0].starts_with("(binary,"))
            .map(|(k, v)| (k.clone(), v[0].clone()))
            .collect()
    }

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
        self.browse.selected_uid()
    }

    /// The latest browse snapshot (rows / selection / scrollbar) for the screen.
    pub fn browse(&self) -> &super::browse::Snapshot { &self.browse.snapshot }
    /// Whether a browse fetch is in flight (drives the `loading…` hint).
    pub fn browse_loading(&self) -> bool { self.browse.loading }
    /// Whether the last browse fetch errored (surfaced in the header).
    pub fn browse_err(&self) -> bool { self.browse.snapshot.err }
    /// Whether the browse is true server-sorted (VLV) paging (scales to millions)
    /// vs. the capped client-sorted fallback used when the server lacks it.
    pub fn browse_keyset(&self) -> bool { self.session().caps.vlv }

    /// Refresh the browse list after a write, preserving the selected uid (the
    /// worker rebuilds; there is no in-place refresh).
    fn rebuild_user_list(&mut self) {
        let keep = self.browse.selected_key();
        self.browse.rebuild(keep);
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

pub fn run(sessions: Vec<Session>, write_mode: bool, dry_run: bool, cfg: Config, password: Option<String>) -> anyhow::Result<()> {
    let mut app = App::new(sessions, write_mode, dry_run, cfg, password);
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
        // Pick up any browse window the worker produced (non-blocking); load the
        // detail of whatever is now selected (no-op if unchanged).
        app.browse.poll();
        if app.mode == Mode::Browse {
            app.ensure_detail_loaded();
        }
        // Fire the debounced server search once typing has settled (never per keystroke).
        app.tick_search();
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
        Event::Key(key)   => handle_key(app, key.code, key.modifiers),
        Event::Mouse(me)  => { handle_mouse(app, me.kind); Ok(false) }
        Event::Paste(text) => handle_paste(app, text),
        _ => Ok(false),
    }
}

/// A bracketed paste (one atomic block) is delivered to the active text field —
/// never re-interpreted as command keystrokes. With no overlay, it feeds the search
/// query while searching, and is otherwise ignored.
fn handle_paste(app: &mut App, text: String) -> anyhow::Result<bool> {
    if let Some(ov) = &mut app.overlay {
        match ov.handle_paste(&text) {
            OverlayResult::Stay   => {}
            OverlayResult::Cancel => app.overlay = None,
            OverlayResult::Commit(action) => {
                app.overlay = None;
                perform(app, action)?;
            }
        }
        return Ok(false);
    }
    if app.mode == Mode::Search {
        app.paste_search(&text);
    }
    Ok(false)
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
                if down { app.browse.select_next(); } else { app.browse.select_prev(); }
            }
            Pane::Right => {
                app.detail_scroll = if down {
                    app.detail_scroll.saturating_add(1)
                } else {
                    app.detail_scroll.saturating_sub(1)
                };
            }
        },
        Mode::GroupSelect => match app.group_browse_focus {
            Pane::Left => {
                if down { app.groups_cur.down(app.groups().len()); } else { app.groups_cur.up(); }
                app.reset_group_detail();
            }
            Pane::Right => {
                app.group_detail_scroll = if down {
                    app.group_detail_scroll.saturating_add(1)
                } else {
                    app.group_detail_scroll.saturating_sub(1)
                };
            }
        },
        Mode::Membership => match app.active_pane {
            Pane::Left  => if down { app.left_cur.down(app.users().len()); } else { app.left_cur.up() },
            Pane::Right => if down { app.right_cur.down(app.member_list().len()); } else { app.right_cur.up() },
        },
        Mode::Dit => {
            if down { app.dit_cur.down(app.dit_rows.len()); } else { app.dit_cur.up(); }
            app.load_dit_detail();
        }
        Mode::Search => {
            if down { app.search_cur.down(app.search_hits.len()); } else { app.search_cur.up(); }
        }
    }
}

fn update_offsets(app: &mut App, area: Rect) {
    // border(2) + header(1) + sep(1) = 4
    let vis = area.height.saturating_sub(4) as usize;
    if vis == 0 { return; }

    let nusers = app.users().len();
    let ngroups = app.groups().len();
    // Browse list: track the body height, then read its (estimated) scrollbar.
    app.browse.set_viewport(vis);
    app.groups_cur.keep_in_view(ngroups, vis);
    app.left_cur.keep_in_view(nusers, vis);

    let rlen = app.member_uids().len();
    app.right_cur.clamp(rlen);
    app.right_cur.keep_in_view(rlen, vis);

    let ndit = app.dit_rows.len();
    app.dit_cur.clamp(ndit);
    app.dit_cur.keep_in_view(ndit, vis);

    // Search results list uses the whole inner area minus the query line + separator.
    let nsearch = app.search_hits.len();
    let search_vis = vis.saturating_sub(2).max(1);
    app.search_cur.clamp(nsearch);
    app.search_cur.keep_in_view(nsearch, search_vis);

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

    // Group detail pane (no portrait band): keep the selected attribute in view and
    // clamp its scroll, mirroring the user detail pane above.
    let gdetail_vis = vis.saturating_sub(2).max(1);
    if app.mode == Mode::GroupSelect && app.group_browse_focus == Pane::Right {
        let ntargets = screens::group_detail::edit_targets(app).len();
        if ntargets > 0 && app.group_detail_cur >= ntargets { app.group_detail_cur = ntargets - 1; }
        if let Some(row) = screens::group_detail::target_row(app, app.group_detail_cur) {
            if row < app.group_detail_scroll {
                app.group_detail_scroll = row;
            } else if row >= app.group_detail_scroll + gdetail_vis {
                app.group_detail_scroll = row + 1 - gdetail_vis;
            }
        }
    }
    let g_max = screens::group_detail::row_count(app).saturating_sub(gdetail_vis);
    if app.group_detail_scroll > g_max { app.group_detail_scroll = g_max; }
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

    // `/` opens cross-directory search from any screen (read-only; no write gate).
    // Suppressed while already searching, so `/` is a literal query character there.
    if key == Char('/') && app.mode != Mode::Search {
        app.open_search();
        return Ok(false);
    }

    app.status = None;

    match app.mode {
        Mode::Browse => match (app.browse_focus, key) {
            (_, Char('q')) => return Ok(true),
            // Esc steps focus back to the list, then quits.
            (Pane::Right, Esc) => app.browse_focus = Pane::Left,
            (Pane::Left,  Esc) => return Ok(true),
            (_, Char('g')) => {
                app.mode = Mode::GroupSelect;
                app.groups_cur.reset();
                app.group_browse_focus = Pane::Left;
                app.reset_group_detail();
            }
            (_, Char('t')) => { app.mode = Mode::Dit; app.enter_dit(); }
            (_, Char('n')) => open_new_user(app),
            (_, Char('D')) => open_delete_user(app),
            (_, Tab) | (_, BackTab) => {
                app.browse_focus =
                    if app.browse_focus == Pane::Left { Pane::Right } else { Pane::Left };
            }
            // Left pane: navigate the windowed user list (reloads the detail record).
            (Pane::Left, Up   | Char('k')) => app.browse.select_prev(),
            (Pane::Left, Down | Char('j')) => app.browse.select_next(),
            (Pane::Left, PageUp)   => app.browse.page_up(),
            (Pane::Left, PageDown) => app.browse.page_down(),
            // Right pane: move the editable-attribute cursor.
            (Pane::Right, Up   | Char('k')) => { app.detail_cur = app.detail_cur.saturating_sub(1); }
            (Pane::Right, Down | Char('j')) => {
                let n = screens::detail::edit_targets(app).len();
                if app.detail_cur + 1 < n { app.detail_cur += 1; }
            }
            (Pane::Right, Char('e')) => open_attr_edit(app),
            (Pane::Right, Char('E')) => open_attr_bigedit(app),
            (Pane::Right, Char('K')) => open_key_editor(app),
            (Pane::Right, Char('p')) => open_passwd(app),
            _ => {}
        },

        Mode::GroupSelect => match (app.group_browse_focus, key) {
            (_, Char('q')) => return Ok(true),
            (Pane::Right, Esc) => app.group_browse_focus = Pane::Left,
            (Pane::Left,  Esc) => app.mode = Mode::Browse,
            (_, Tab) | (_, BackTab) => {
                app.group_browse_focus =
                    if app.group_browse_focus == Pane::Left { Pane::Right } else { Pane::Left };
            }
            // Actions valid from either pane (operate on the cursored group).
            (_, Char('n')) => open_new_group(app),
            (_, Char('D')) => open_delete_group(app),
            (_, Char('a')) => open_remove_alias(app),
            (_, Char('r')) => open_rename_group(app),
            // Left pane: navigate the group list.
            (Pane::Left, Up   | Char('k')) => { app.groups_cur.up();                     app.reset_group_detail(); }
            (Pane::Left, Down | Char('j')) => { app.groups_cur.down(app.groups().len());  app.reset_group_detail(); }
            (Pane::Left, PageUp)   => { app.groups_cur.page(-10, app.groups().len()); app.reset_group_detail(); }
            (Pane::Left, PageDown) => { app.groups_cur.page(10, app.groups().len());  app.reset_group_detail(); }
            (Pane::Left, Enter) => {
                app.selected_group = app.groups_cur.cursor;
                app.mode = Mode::Membership;
                app.active_pane = Pane::Left;
                app.left_cur.reset();
                app.right_cur.reset();
            }
            // Right pane: move the editable-attribute cursor / edit.
            (Pane::Right, Up   | Char('k')) => { app.group_detail_cur = app.group_detail_cur.saturating_sub(1); }
            (Pane::Right, Down | Char('j')) => {
                let n = screens::group_detail::edit_targets(app).len();
                if app.group_detail_cur + 1 < n { app.group_detail_cur += 1; }
            }
            (Pane::Right, Char('e')) => open_group_attr_edit(app),
            (Pane::Right, Char('E')) => open_group_attr_bigedit(app),
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

        Mode::Dit => match (app.dit_focus, key) {
            (_, Char('q')) => return Ok(true),
            (Pane::Right, Esc) => app.dit_focus = Pane::Left,
            (Pane::Left,  Esc) => app.mode = Mode::Browse,
            (_, Tab) | (_, BackTab) => {
                app.dit_focus = if app.dit_focus == Pane::Left { Pane::Right } else { Pane::Left };
            }
            // Left pane: navigate/expand the tree.
            (Pane::Left, Up   | Char('k')) => { app.dit_cur.up();                      app.load_dit_detail(); }
            (Pane::Left, Down | Char('j')) => { app.dit_cur.down(app.dit_rows.len());   app.load_dit_detail(); }
            (Pane::Left, PageUp)   => { app.dit_cur.page(-10, app.dit_rows.len()); app.load_dit_detail(); }
            (Pane::Left, PageDown) => { app.dit_cur.page(10, app.dit_rows.len());  app.load_dit_detail(); }
            (Pane::Left, Enter | Char('l') | Right) => app.dit_toggle(),
            (Pane::Left, Char('h') | Left) => app.dit_collapse(),
            // Entry actions (either pane): delete, edit an attribute.
            (_, Char('D')) => open_dit_delete(app),
            (Pane::Right, Up   | Char('k')) => app.dit_detail_cur = app.dit_detail_cur.saturating_sub(1),
            (Pane::Right, Down | Char('j')) => {
                let n = app.dit_edit_targets().len();
                if app.dit_detail_cur + 1 < n { app.dit_detail_cur += 1; }
            }
            (Pane::Right, Char('e')) => open_dit_attr_edit(app, false),
            (Pane::Right, Char('E')) => open_dit_attr_edit(app, true),
            _ => {}
        },

        Mode::Search => match key {
            Esc => app.mode = app.search_from,
            Up       => app.search_cur.up(),
            Down     => app.search_cur.down(app.search_hits.len()),
            PageUp   => app.search_cur.page(-10, app.search_hits.len()),
            PageDown => app.search_cur.page(10, app.search_hits.len()),
            Enter => {
                app.flush_search(); // act on fresh results if a debounced search was pending
                if let Some(hit) = app.search_hits.get(app.search_cur.cursor).cloned() {
                    match hit.kind {
                        HitKind::User => {
                            app.mode = Mode::Browse;
                            app.browse_focus = Pane::Left;
                            app.select_user(&hit.key);
                        }
                        HitKind::Group => {
                            app.mode = Mode::GroupSelect;
                            app.group_browse_focus = Pane::Left;
                            app.select_group_by_dn(&hit.key);
                            app.reset_group_detail();
                        }
                    }
                } else {
                    app.mode = app.search_from;
                }
            }
            // Everything else edits the query field (chars, Backspace, Left/Right, …).
            _ => {
                if mullion::line_edit(&mut app.search_query, &mut app.search_caret, key) {
                    app.recompute_search();
                }
            }
        },
    }
    Ok(false)
}

/// Open a typed-DN delete confirmation for the DIT-selected entry.
fn open_dit_delete(app: &mut App) {
    if !app.can_write_ui() {
        app.status = Some(("Read-only — pass --write to modify".into(), true));
        return;
    }
    let Some(dn) = app.dit_selected_dn() else { return; };
    let label = app.dit_rows().get(app.dit_cur.cursor).map(|r| r.label.clone()).unwrap_or_else(|| dn.clone());
    let prompt = format!("Delete {label}? Irreversible.");
    let action = Action::DeleteEntry { dn: dn.clone(), label };
    app.overlay = Some(Overlay::Confirm(overlay::ConfirmDialog::typed_dn(prompt, dn, action)));
}

/// Edit the DIT-selected entry's cursored attribute (single-line or `big` textarea).
fn open_dit_attr_edit(app: &mut App, big: bool) {
    if !app.can_write_ui() {
        app.status = Some(("Read-only — pass --write to modify".into(), true));
        return;
    }
    let targets = app.dit_edit_targets();
    let Some((attr, value)) = targets.get(app.dit_detail_cur).cloned() else { return; };
    let Some(dn) = app.dit_selected_dn() else { return; };
    app.overlay = Some(if big {
        Overlay::TextArea(overlay::TextAreaDialog::edit_attr(dn, attr, &value))
    } else {
        Overlay::Input(overlay::InputDialog::edit_attr(dn, attr, &value))
    });
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

/// Open the multi-line "big edit" textarea for the detail pane's selected attribute.
fn open_attr_bigedit(app: &mut App) {
    if !app.can_write_ui() {
        app.status = Some(("Read-only — pass --write to modify".into(), true));
        return;
    }
    let targets = screens::detail::edit_targets(app);
    let Some(target) = targets.get(app.detail_cur) else { return; };
    let dn = match app.detail() { Some(u) => u.dn.clone(), None => return };
    let dlg = overlay::TextAreaDialog::edit_attr(dn, target.attr.clone(), &target.value);
    app.overlay = Some(Overlay::TextArea(dlg));
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
    let Some(user) = app.browse.snapshot.selected_user() else { return; };
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

/// Open the attribute editor for the group detail pane's selected target.
fn open_group_attr_edit(app: &mut App) {
    if !app.can_write_ui() {
        app.status = Some(("Read-only — pass --write to modify".into(), true));
        return;
    }
    let targets = screens::group_detail::edit_targets(app);
    let Some(target) = targets.get(app.group_detail_cur) else { return; };
    let Some(group) = app.groups().get(app.groups_cur.cursor) else { return; };
    let dlg = overlay::InputDialog::edit_attr(group.dn.clone(), target.attr.clone(), &target.value);
    app.overlay = Some(Overlay::Input(dlg));
}

/// Open the multi-line "big edit" textarea for the group detail pane's selected attribute.
fn open_group_attr_bigedit(app: &mut App) {
    if !app.can_write_ui() {
        app.status = Some(("Read-only — pass --write to modify".into(), true));
        return;
    }
    let targets = screens::group_detail::edit_targets(app);
    let Some(target) = targets.get(app.group_detail_cur) else { return; };
    let Some(group) = app.groups().get(app.groups_cur.cursor) else { return; };
    let dlg = overlay::TextAreaDialog::edit_attr(group.dn.clone(), target.attr.clone(), &target.value);
    app.overlay = Some(Overlay::TextArea(dlg));
}

/// Open the rename dialog (cn/RDN via modrdn) for the cursored group.
fn open_rename_group(app: &mut App) {
    if !app.can_write_ui() {
        app.status = Some(("Read-only — pass --write to modify".into(), true));
        return;
    }
    let Some(group) = app.groups().get(app.groups_cur.cursor) else { return; };
    let dlg = overlay::InputDialog::rename_group(group.dn.clone(), &group.name);
    app.overlay = Some(Overlay::Input(dlg));
}

/// Open a confirmation to remove the cursored group's first alias `cn` (an extra,
/// non-RDN name). Undoable. Repeat to strip multiple aliases.
fn open_remove_alias(app: &mut App) {
    if !app.can_write_ui() {
        app.status = Some(("Read-only — pass --write to modify".into(), true));
        return;
    }
    let Some(group) = app.groups().get(app.groups_cur.cursor) else { return; };
    let Some(alias) = group.aliases.first().cloned() else {
        app.status = Some(("This group has no extra cn to remove".into(), false));
        return;
    };
    let dn   = group.dn.clone();
    let name = group.name.clone();
    let more = if group.aliases.len() > 1 {
        format!(" ({} more after this)", group.aliases.len() - 1)
    } else {
        String::new()
    };
    let prompt = format!("Remove extra name cn={alias} from {name}?{more}");
    let action = Action::RemoveAlias { dn, alias, group: name };
    app.overlay = Some(Overlay::Confirm(overlay::ConfirmDialog::yes_no(prompt, action)));
}

// ─── write chokepoint ─────────────────────────────────────────────────────────

/// One-line description of the LDAP operation an action would perform (dry-run).
pub(crate) fn describe_action(action: &Action) -> String {
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
        Action::RenameGroup { dn, new_cn, .. } =>
            format!("RENAME {dn} → cn={new_cn}"),
        Action::RemoveAlias { dn, alias, .. } =>
            format!("DELETE cn={alias} on {dn}"),
        Action::AddAlias { dn, alias, .. } =>
            format!("ADD cn={alias} on {dn}"),
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
        // A write from (or undone within) the DIT browser must refresh the tree.
        if app.mode == Mode::Dit {
            app.refresh_dit();
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
                    // The row may show an edited field (e.g. cn); refresh the browse window.
                    app.rebuild_user_list();
                    // The edited entry may be a group (group detail reads the cache).
                    let _ = app.session_mut().refresh_groups();
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
        Action::RenameGroup { dn, new_cn, old_name } => {
            match app.session_mut().client.rename_entry(dn, new_cn) {
                Ok(new_dn) => { app.session_mut().refresh_groups()?; app.select_group_by_dn(&new_dn); app.status = Some((format!("Renamed {old_name} → {new_cn}"), false)); true }
                Err(e) => { app.status = Some((format!("Error: {e}"), true)); false }
            }
        }
        Action::RemoveAlias { dn, alias, group } => {
            match app.session_mut().client.modify_delete(dn, "cn", &[alias.as_str()]) {
                Ok(()) => { app.session_mut().refresh_groups()?; app.select_group_by_dn(dn); app.status = Some((format!("Removed alias {alias} from {group}"), false)); true }
                Err(e) => { app.status = Some((format!("Error: {e}"), true)); false }
            }
        }
        Action::AddAlias { dn, alias, group } => {
            match app.session_mut().client.modify_add(dn, "cn", &[alias.as_str()]) {
                Ok(()) => { app.session_mut().refresh_groups()?; app.select_group_by_dn(dn); app.status = Some((format!("Restored alias {alias} on {group}"), false)); true }
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
        Action::RenameGroup { dn, new_cn, old_name } => {
            // After the rename the entry lives at cn=<new_cn>,<tail>; the inverse
            // renames that back to the old name.
            let tail = dn.split_once(',').map(|(_, r)| r).unwrap_or("");
            let new_dn = if tail.is_empty() { format!("cn={new_cn}") } else { format!("cn={new_cn},{tail}") };
            Some((
                format!("rename {old_name} → {new_cn}"),
                Action::RenameGroup { dn: new_dn, new_cn: old_name.clone(), old_name: new_cn.clone() },
            ))
        }
        Action::RemoveAlias { dn, alias, group } => Some((
            format!("remove alias {alias} from {group}"),
            Action::AddAlias { dn: dn.clone(), alias: alias.clone(), group: group.clone() },
        )),
        Action::AddAlias { dn, alias, group } => Some((
            format!("add alias {alias} to {group}"),
            Action::RemoveAlias { dn: dn.clone(), alias: alias.clone(), group: group.clone() },
        )),
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

    /// Jump the browse cursor to `uid` (seeking the server if needed) and load detail.
    fn select_user(&mut self, uid: &str) {
        let key = self.browse_paging_key(uid);
        self.browse.select_key(&key);
        self.load_detail(None);
        self.ensure_detail_loaded();
    }

    /// The browse **paging key** for a uid: the uid itself when the browse is sorted by
    /// `uid` (the common case, no round-trip); otherwise the value of the configured
    /// sort attribute (e.g. `sortRank`), looked up via `get_user`. Falls back to the uid
    /// if the lookup fails or the entry lacks the attribute — the jump may then miss,
    /// but nothing breaks.
    fn browse_paging_key(&mut self, uid: &str) -> String {
        if self.browse_sort_attr == "uid" {
            return uid.to_string();
        }
        let attr = self.browse_sort_attr.clone();
        match self.session_mut().client.get_user(uid) {
            Ok(Some(u)) => u.attrs.get(&attr)
                .and_then(|v| v.first())
                .cloned()
                .unwrap_or_else(|| uid.to_string()),
            _ => uid.to_string(),
        }
    }

    /// Rebuild the browse list after a delete and reload detail (the cursor lands on
    /// the neighbour the rebuild keeps in view).
    fn clamp_and_reload_detail(&mut self) {
        self.rebuild_user_list();
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

    /// Reset the group detail pane's cursor/scroll (on a group-list navigation).
    fn reset_group_detail(&mut self) {
        self.group_detail_cur = 0;
        self.group_detail_scroll = 0;
    }

    // ── cross-directory search ────────────────────────────────────────────────

    /// Enter search mode: remember where we came from, clear the query and results.
    /// Read-only — usable even without `--write`.
    fn open_search(&mut self) {
        self.search_from = self.mode;
        self.mode = Mode::Search;
        self.search_query.clear();
        self.search_caret = 0;
        self.search_hits.clear();
        self.search_cur.reset();
        self.search_dirty = false;
    }

    /// Mark the query dirty; the actual (blocking) server search runs after a short
    /// debounce (see [`tick_search`](Self::tick_search)), so typing on a huge server
    /// doesn't fire a 0.3–1.3 s query per keystroke and freeze the UI.
    fn recompute_search(&mut self) {
        self.search_dirty = true;
        self.search_last_edit = Instant::now();
    }

    /// Run the pending search once the query has settled (called each frame).
    fn tick_search(&mut self) {
        if self.search_dirty && self.search_last_edit.elapsed() >= Duration::from_millis(250) {
            self.run_search();
            self.search_dirty = false;
        }
    }

    /// Run any pending search *now* — used on Enter so it acts on fresh results.
    fn flush_search(&mut self) {
        if self.search_dirty {
            self.run_search();
            self.search_dirty = false;
        }
    }

    /// The server-side query: a bounded filter (scales to millions), then rank the
    /// small result set with the same score()/search_hits logic.
    fn run_search(&mut self) {
        let q = self.search_query.trim().to_string();
        self.search_cur.reset();
        if q.len() < 2 {
            self.search_hits.clear();
            return;
        }
        let users = self.session_mut().client.search_users(&q, 200).unwrap_or_default();
        let groups = self.session_mut().client.search_groups(&q, 200).unwrap_or_default();
        self.search_hits = search_hits(&users, &groups, &q);
    }

    /// Feed a bracketed paste into the query field (single line); the debounced
    /// search picks it up.
    pub fn paste_search(&mut self, text: &str) {
        overlay::paste_into(&mut self.search_query, &mut self.search_caret, text, false);
        self.recompute_search();
    }

    pub fn search_query(&self) -> &str { &self.search_query }
    pub fn search_caret(&self) -> usize { self.search_caret }
    pub fn search_hits(&self) -> &[SearchHit] { &self.search_hits }
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
        Mode::Dit         => screens::dit::render(app, buf, main),
        Mode::Search      => screens::search::render(app, buf, main),
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

// ─── search matching ───────────────────────────────────────────────────────────

/// Cap on results, so a 1-character query can't build a giant list.
pub(crate) const MAX_HITS: usize = 300;

/// Case-insensitive field score: `0` exact, `1` prefix, `2` substring, `None` when
/// it doesn't occur. `needle` must already be lowercased and non-empty.
fn score(hay: &str, needle: &str) -> Option<u8> {
    let h = hay.to_lowercase();
    if h == needle { Some(0) }
    else if h.starts_with(needle) { Some(1) }
    else if h.contains(needle) { Some(2) }
    else { None }
}

/// Best (lowest) score of `needle` across `fields`; `None` if it matches none.
fn best(needle: &str, fields: &[&str]) -> Option<u8> {
    fields.iter().filter_map(|f| score(f, needle)).min()
}

/// Match `query` against users and groups by first/last name, account name (uid),
/// group name, uidNumber and gidNumber, returning display-ready hits sorted by rank
/// (exact < prefix < substring) then label, capped at [`MAX_HITS`].
pub(crate) fn search_hits(users: &[User], groups: &[Group], query: &str) -> Vec<SearchHit> {
    let q = query.trim().to_lowercase();
    if q.is_empty() { return Vec::new(); }

    let mut hits: Vec<SearchHit> = Vec::new();

    for u in users {
        let uidn = u.uid_number.to_string();
        let gidn = u.gid_number.to_string();
        let fields = [
            u.uid.as_str(), u.cn.as_str(), u.given_name.as_str(),
            u.sn.as_str(), uidn.as_str(), gidn.as_str(),
        ];
        if let Some(rank) = best(&q, &fields) {
            hits.push(SearchHit {
                kind: HitKind::User,
                key: u.uid.clone(),
                primary: u.uid.clone(),
                secondary: format!("{}  ·  uid {}  gid {}", u.cn, u.uid_number, u.gid_number),
                rank,
            });
        }
    }

    for g in groups {
        let gidn = g.gid_number.map(|n| n.to_string());
        let mut fields: Vec<&str> = vec![g.name.as_str()];
        fields.extend(g.aliases.iter().map(String::as_str));
        if let Some(s) = gidn.as_deref() { fields.push(s); }
        if let Some(rank) = best(&q, &fields) {
            let gid = gidn.as_deref().unwrap_or("—");
            let aka = if g.aliases.is_empty() { String::new() }
                      else { format!("  aka {}", g.aliases.join(",")) };
            hits.push(SearchHit {
                kind: HitKind::Group,
                key: g.dn.clone(),
                primary: g.name.clone(),
                secondary: format!("gid {}  ·  {} members{}", gid, g.members.len(), aka),
                rank,
            });
        }
    }

    hits.sort_by(|a, b| {
        a.rank.cmp(&b.rank)
            .then_with(|| a.primary.to_lowercase().cmp(&b.primary.to_lowercase()))
    });
    hits.truncate(MAX_HITS);
    hits
}

#[cfg(test)]
mod search_tests {
    use super::*;
    use std::collections::HashMap;

    fn user(uid: &str, cn: &str, given: &str, sn: &str, uidn: u32, gidn: u32) -> User {
        User {
            dn: format!("uid={uid},ou=users,dc=example"),
            uid: uid.into(), cn: cn.into(),
            sn: sn.into(), given_name: given.into(),
            uid_number: uidn, gid_number: gidn,
            home: String::new(), shell: String::new(),
            ssh_keys: Vec::new(), photo: None, sort_key: String::new(), attrs: HashMap::new(),
        }
    }
    fn group(name: &str, gid: Option<u32>, aliases: &[&str]) -> Group {
        Group {
            dn: format!("cn={name},ou=groups,dc=example"),
            name: name.into(),
            aliases: aliases.iter().map(|s| s.to_string()).collect(),
            gid_number: gid, members: Vec::new(),
            dup_name: false, dup_gid: false, attrs: HashMap::new(),
        }
    }

    #[test]
    fn empty_query_yields_nothing() {
        let us = [user("quixote", "Alonso Quijano", "Alonso", "Quijano", 1001, 100)];
        assert!(search_hits(&us, &[], "").is_empty());
    }

    #[test]
    fn matches_first_name_and_returns_user() {
        let us = [user("quixote", "Alonso Quijano", "Alonso", "Quijano", 1001, 100)];
        let hits = search_hits(&us, &[], "alon");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].kind, HitKind::User);
        assert_eq!(hits[0].key, "quixote");
    }

    #[test]
    fn matches_last_name_and_uidnumber() {
        let us = [user("quixote", "Alonso Quijano", "Alonso", "Quijano", 1001, 100)];
        assert_eq!(search_hits(&us, &[], "quijano").len(), 1);
        assert_eq!(search_hits(&us, &[], "1001").len(), 1);
    }

    #[test]
    fn matches_group_by_name_and_gidnumber() {
        let gs = [group("knights-errant", Some(5000), &["windmillfighters"])];
        let by_name = search_hits(&[], &gs, "knight");
        assert_eq!(by_name.len(), 1);
        assert_eq!(by_name[0].kind, HitKind::Group);
        assert_eq!(by_name[0].key, "cn=knights-errant,ou=groups,dc=example");
        assert_eq!(search_hits(&[], &gs, "5000").len(), 1);   // gidNumber
        assert_eq!(search_hits(&[], &gs, "windmill").len(), 1); // alias
    }

    #[test]
    fn ranks_exact_and_prefix_before_substring() {
        let us = [
            user("bob",   "Bob Carob",  "Bob",   "Carob", 10, 100), // "rob" only mid-word (substring)
            user("robby", "Robby Zzz",  "Robby", "Zzz",   11, 100), // "rob" prefix of uid
            user("rob",   "Rob Aaa",    "Rob",   "Aaa",   12, 100), // "rob" exact uid
        ];
        let hits = search_hits(&us, &[], "rob");
        assert_eq!(hits.len(), 3);
        assert_eq!(hits[0].key, "rob");   // exact first
        assert_eq!(hits[1].key, "robby"); // then prefix
        assert_eq!(hits[2].key, "bob");   // then substring
    }
}
