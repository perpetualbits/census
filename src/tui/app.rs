//! TUI orchestrator: application state, event loop, key routing, render dispatch.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use crossterm::event::{Event, MouseEventKind};
use mullion::{
    backend::CrosstermBackend, Buffer, EventReader, KeyCode, KeyModifiers, Rect, Terminal,
};

use crate::config::ConnMode;
use anyhow::Context;

use crate::ldap::client::{Brand, DitNode, Group, LdapClient, SchemaElem, SchemaKind, User};
use crate::session::Session;

use super::backup::Backups;
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
enum Mode { Browse, GroupSelect, Membership, Dit, Search, Schema }

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

/// Per-session UI state that must follow the focused connection. The *focused*
/// session's copy lives directly in the [`App`] fields; the others are stashed here
/// and swapped in by [`App::focus_session`]. `browse` is `None` until a session is
/// focused for the first time (then spawned and retained, so switching back is instant).
#[derive(Default)]
struct SessionUi {
    browse: Option<Browse>,
    detail: Option<User>,
    detail_photo: Option<mullion::video::Frame>,
    dit_children: HashMap<String, Vec<DitNode>>,
    dit_truncated: HashSet<String>,
    dit_expanded: HashSet<String>,
    dit_rows: Vec<DitRow>,
    dit_detail: Vec<(String, Vec<String>)>,
}

/// One flattened row of the connections rail: a server group (parent) or a domain
/// (leaf, carrying its session index). Mirrors [`DitRow`] so it renders through the
/// same `mullion::render_tree_row`.
pub struct RailRow {
    pub session_idx: Option<usize>, // Some for a domain leaf, None for a server group
    pub label: String,
    pub ancestor_last: Vec<bool>,
    pub is_last: bool,
    pub expanded: Option<bool>,     // server: Some(expanded); domain leaf: None
    pub is_server: bool,
}

/// A pending host-provisioning operation for an OpenLDAP domain: the filesystem step
/// (create/remove the backend's directory) runs via the server's `provision_cmd`, then
/// census does the `cn=config` LDAP steps. Held until the operator confirms the review.
struct ProvisionPlan {
    session_idx: usize, // a session on the target server (its cfg + creds)
    suffix: String,
    dir: String,        // the backend's on-disk directory
    fs_script: String,  // the mkdir+chown to run on the host
}

/// A pending entry migration: copy `dn` from the focused connection (`source`) to each
/// `target` connection, rebasing the DN onto the target's base. Held until confirmed.
struct MigrationPlan {
    source: usize,
    targets: Vec<usize>,
    dn: String,
    label: String,
}

pub struct App {
    sessions: Vec<Session>,
    /// The connection whose data fills the workspace (its browse/DIT/detail live in the
    /// App fields below; the other sessions' equivalents are stashed in `session_ui`).
    focused:  usize,
    /// Connections marked "active" for set operations (compare/migrate — Phase 3).
    marked:   HashSet<usize>,
    /// Stashed per-session UI state for the *non-focused* sessions; swapped with the
    /// live App fields on `focus_session`. `session_ui[focused]` is an empty placeholder.
    session_ui: Vec<SessionUi>,
    /// Connections rail (left sidebar): cursor row, whether it holds key focus, and
    /// which server groups are expanded.
    rail_cur: ListCursor,
    rail_focused: bool,
    rail_expanded: HashSet<String>,
    rail_rows: Vec<RailRow>,
    /// A mode change awaiting confirmation: `(session index, new mode)`.
    pending_mode: Option<(usize, ConnMode)>,
    /// A host-provisioning plan awaiting the operator's review/confirmation.
    pending_provision: Option<ProvisionPlan>,
    /// A migration awaiting the operator's review/confirmation.
    pending_migration: Option<MigrationPlan>,
    /// In-flight domain backups (LDIF export) running on background threads.
    backups: Backups,
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

    // Schema browser (per-server; opened from the rail with `s`).
    schema_elems: Vec<SchemaElem>,
    pub schema_cur: ListCursor,
    pub schema_focus: Pane,
    schema_filter: String,
    schema_filtering: bool,     // typing edits the filter (started with `/`)
    schema_from: Mode,          // mode to return to on Esc
    schema_title: String,       // "<server> (<brand>)"
    schema_session: usize,      // the session the schema was read from (for add)

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

    anim_start: Instant,
    anim_on: bool,

    /// Rollback journal (on-disk LDIF) + in-app undo stack.
    journal: Journal,
    /// Whether the bottom LDIF change-preview tile is shown.
    show_preview: bool,
}

impl App {
    fn new(sessions: Vec<Session>) -> Self {
        // The browse list runs on its own thread with its own read-only connection,
        // built from the focused session's config/secret (session 0 at startup).
        let s0 = &sessions[0];
        let browse_sort_attr = s0.cfg.browse.sort_attr.clone();
        let browse = Browse::spawn(s0.cfg.clone(), s0.password.clone(), 20);
        let session_ui = (0..sessions.len()).map(|_| SessionUi::default()).collect();
        Self {
            sessions, focused: 0, mode: Mode::Browse,
            marked: HashSet::new(),
            session_ui,
            rail_cur: ListCursor::new(),
            rail_focused: false,
            rail_expanded: HashSet::new(),
            rail_rows: Vec::new(),
            pending_mode: None,
            pending_provision: None,
            pending_migration: None,
            backups: Backups::new(),
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
            schema_elems: Vec::new(),
            schema_cur: ListCursor::new(),
            schema_focus: Pane::Left,
            schema_filter: String::new(),
            schema_filtering: false,
            schema_from: Mode::Browse,
            schema_title: String::new(),
            schema_session: 0,
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
            status: None,
            anim_start: Instant::now(),
            anim_on: true,
            journal: Journal::new(),
            show_preview: false,
        }
    }

    /// May the write UI be opened on the focused connection? True in write or dry-run.
    pub fn can_write_ui(&self) -> bool { self.session().mode.can_write() }

    /// Short label of the focused connection's mode, for the title bar.
    pub fn mode_tag(&self) -> &'static str { self.session().mode.tag() }

    // ── active session ──────────────────────────────────────────────────────

    fn session(&self) -> &Session { &self.sessions[self.focused] }
    fn session_mut(&mut self) -> &mut Session { &mut self.sessions[self.focused] }

    /// Switch the workspace to session `idx`, stashing the outgoing session's live UI
    /// state (browse worker, DIT tree, detail) into its slot and restoring the
    /// incoming session's — lazily spawning its browse worker the first time, then
    /// retaining it so switching back is instant.
    fn focus_session(&mut self, idx: usize) {
        if idx == self.focused || idx >= self.sessions.len() { return; }
        let old = self.focused;
        // The new session's browse: reuse its retained worker, or spawn one now. The
        // per-frame `set_viewport` corrects the seed viewport immediately.
        let new_browse = match self.session_ui[idx].browse.take() {
            Some(b) => b,
            None => {
                let s = &self.sessions[idx];
                Browse::spawn(s.cfg.clone(), s.password.clone(), 20)
            }
        };
        // Stash outgoing live state.
        self.session_ui[old] = SessionUi {
            browse: Some(std::mem::replace(&mut self.browse, new_browse)),
            detail: self.detail.take(),
            detail_photo: self.detail_photo.take(),
            dit_children: std::mem::take(&mut self.dit_children),
            dit_truncated: std::mem::take(&mut self.dit_truncated),
            dit_expanded: std::mem::take(&mut self.dit_expanded),
            dit_rows: std::mem::take(&mut self.dit_rows),
            dit_detail: std::mem::take(&mut self.dit_detail),
        };
        // Restore incoming stashed state (browse already swapped above).
        let ui = std::mem::take(&mut self.session_ui[idx]);
        self.detail = ui.detail;
        self.detail_photo = ui.detail_photo;
        self.dit_children = ui.dit_children;
        self.dit_truncated = ui.dit_truncated;
        self.dit_expanded = ui.dit_expanded;
        self.dit_rows = ui.dit_rows;
        self.dit_detail = ui.dit_detail;
        self.browse_sort_attr = self.sessions[idx].cfg.browse.sort_attr.clone();
        self.focused = idx;
        // Reset transient cursors (not stashed per-session).
        self.detail_scroll = 0;
        self.detail_cur = 0;
        self.dit_cur = ListCursor::new();
        self.dit_detail_cur = 0;
        self.browse_focus = Pane::Left;
        self.status = Some((format!("→ {}", self.session().label()), false));
        if self.mode == Mode::Dit {
            self.enter_dit();
        }
    }

    /// Expand every server group and place the rail cursor on the focused session.
    fn init_rail(&mut self) {
        for s in &self.sessions {
            self.rail_expanded.insert(s.server_label.clone());
        }
        self.rebuild_rail_rows();
        self.rail_cur.cursor = self.rail_rows.iter()
            .position(|r| r.session_idx == Some(self.focused))
            .unwrap_or(0);
    }

    // ── connections rail accessors (for screens::rail) ───────────────────────
    pub fn rail_rows(&self) -> &[RailRow] { &self.rail_rows }
    pub fn rail_cur(&self) -> &ListCursor { &self.rail_cur }
    pub fn rail_has_focus(&self) -> bool { self.rail_focused }
    pub fn session_count(&self) -> usize { self.sessions.len() }
    pub fn session_mode(&self, i: usize) -> ConnMode { self.sessions[i].mode }
    pub fn is_marked(&self, i: usize) -> bool { self.marked.contains(&i) }
    pub fn focused_idx(&self) -> usize { self.focused }
    pub fn backups_active(&self) -> usize { self.backups.active() }

    // ── connections rail actions (rail-focused key handling) ─────────────────

    /// Enter/Space/l on the cursored row: expand/collapse a server, or focus a domain
    /// (and hop to the workspace).
    fn rail_activate(&mut self) {
        let Some(row) = self.rail_rows.get(self.rail_cur.cursor) else { return; };
        let (is_server, label, sidx) = (row.is_server, row.label.clone(), row.session_idx);
        if is_server {
            self.toggle_server_expand(&label);
        } else if let Some(idx) = sidx {
            self.focus_session(idx);
            self.rail_focused = false;
        }
    }

    /// h/Left on a server row collapses it.
    fn rail_collapse(&mut self) {
        let Some(row) = self.rail_rows.get(self.rail_cur.cursor) else { return; };
        if row.is_server {
            let label = row.label.clone();
            if self.rail_expanded.remove(&label) {
                self.rebuild_rail_rows();
                self.rail_cur.clamp(self.rail_rows.len());
            }
        }
    }

    fn toggle_server_expand(&mut self, label: &str) {
        if !self.rail_expanded.remove(label) {
            self.rail_expanded.insert(label.to_string());
        }
        self.rebuild_rail_rows();
        self.rail_cur.clamp(self.rail_rows.len());
    }

    /// m: toggle the cursored domain in the marked ("active") set.
    fn rail_toggle_mark(&mut self) {
        let Some(row) = self.rail_rows.get(self.rail_cur.cursor) else { return; };
        if let Some(idx) = row.session_idx {
            if !self.marked.remove(&idx) { self.marked.insert(idx); }
            self.status = Some((format!("{} marked", self.marked.len()), false));
        }
    }

    /// M: cycle the cursored domain's mode (read-only → write → dry-run → …). Enabling
    /// live writes waits for y/n confirmation via `pending_mode`.
    fn rail_cycle_mode(&mut self) {
        let Some(row) = self.rail_rows.get(self.rail_cur.cursor) else { return; };
        let Some(idx) = row.session_idx else { return; };
        let next = match self.sessions[idx].mode {
            ConnMode::ReadOnly => ConnMode::Write,
            ConnMode::Write    => ConnMode::DryRun,
            ConnMode::DryRun   => ConnMode::ReadOnly,
        };
        if next == ConnMode::Write {
            self.pending_mode = Some((idx, next));
            self.status = Some((format!("Enable WRITES on {}? (y/n)", self.sessions[idx].label()), true));
        } else {
            self.sessions[idx].mode = next;
            self.status = Some((format!("{} → {}", self.sessions[idx].label(), next.tag()), false));
        }
    }

    /// b: back up (LDIF-export) the cursored domain's full subtree to a file in the
    /// working directory. Read-only, streamed (constant memory), and run on a
    /// background thread with its own connection so a huge domain never blocks the UI;
    /// progress and the final path arrive via [`Backups::poll`].
    fn rail_backup(&mut self) {
        let Some(idx) = self.rail_rows.get(self.rail_cur.cursor).and_then(|r| r.session_idx) else {
            self.status = Some(("move the cursor onto a domain to back it up".into(), true));
            return;
        };
        let (server, domain, base, cfg, password) = {
            let s = &self.sessions[idx];
            (s.server_label.clone(), s.domain_label.clone(), s.client.base_dn.clone(),
             s.cfg.clone(), s.password.clone())
        };
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        let path = std::path::PathBuf::from(format!("{server}-{domain}-{stamp}.ldif"));
        self.backups.start(cfg, password, base, path, domain.clone());
        self.status = Some((format!("backing up {domain} in the background…"), false));
    }

    /// The session to act on for the cursored rail row: the domain's own session, or
    /// (on a server row) the first session belonging to that server.
    fn rail_template_session(&self) -> Option<usize> {
        let row = self.rail_rows.get(self.rail_cur.cursor)?;
        if let Some(idx) = row.session_idx { return Some(idx); }
        self.sessions.iter().position(|s| s.server_label == row.label)
    }

    /// N: prompt to create a new domain on the cursored server. 389-DS or OpenLDAP
    /// (the latter needs config_bind_dn + provision_cmd), Write mode.
    fn rail_new_domain(&mut self) {
        let Some(idx) = self.rail_template_session() else {
            self.status = Some(("no connection here to create a domain on".into(), true));
            return;
        };
        let s = &self.sessions[idx];
        let brand = s.client.brand();
        if brand != Brand::Ds389 && brand != Brand::OpenLdap {
            self.status = Some((format!("creating a domain isn't supported on {}", brand.label()), true));
            return;
        }
        if brand == Brand::OpenLdap && s.cfg.server.config_bind_dn.is_none() {
            self.status = Some(("set config_bind_dn / config_password_cmd (+ provision_cmd) to create a domain on this OpenLDAP server".into(), true));
            return;
        }
        if s.mode != ConnMode::Write {
            self.status = Some(("set this connection to write (M) before creating a domain".into(), true));
            return;
        }
        self.overlay = Some(Overlay::Input(overlay::InputDialog::new_domain(idx)));
    }

    /// Create domain `suffix` on the server of session `template_idx`. 389-DS is pure
    /// LDAP; OpenLDAP also needs a filesystem step on the host (a new backend's
    /// directory), which goes through `provision_cmd` after a review.
    fn do_create_domain(&mut self, template_idx: usize, suffix: &str) {
        if template_idx >= self.sessions.len() { return; }
        if self.sessions[template_idx].mode != ConnMode::Write {
            self.status = Some(("set the connection to write before creating a domain".into(), true));
            return;
        }
        match self.sessions[template_idx].client.brand() {
            Brand::Ds389 => {
                if let Err(e) = self.sessions[template_idx].client.create_domain(suffix) {
                    self.status = Some((format!("create failed: {e:#}"), true));
                    return;
                }
                match self.register_new_domain(template_idx, suffix, None) {
                    Ok(()) => self.status = Some((format!("created domain {suffix}"), false)),
                    Err(e) => self.status = Some((format!("created {suffix}, but connecting it failed: {e:#}"), true)),
                }
            }
            Brand::OpenLdap => self.openldap_create_domain(template_idx, suffix),
            b => self.status = Some((format!("creating a domain isn't supported on {}", b.label()), true)),
        }
    }

    /// Connect a freshly-created domain as its own session and show it in the rail.
    /// `bind_override` sets the new session's bind DN (OpenLDAP: `cn=admin,<suffix>`;
    /// 389-DS keeps the Directory Manager, so `None`).
    fn register_new_domain(&mut self, template_idx: usize, suffix: &str, bind_override: Option<String>) -> anyhow::Result<()> {
        let (mut cfg, password, config_password, pw_source, server_label) = {
            let t = &self.sessions[template_idx];
            (t.cfg.clone(), t.password.clone(), t.config_password.clone(), t.conn.password.clone(), t.server_label.clone())
        };
        cfg.server.base_dn = suffix.to_string();
        if let Some(b) = bind_override { cfg.server.bind_dn = Some(b); }
        let domain_label = crate::config::domain_label(suffix);
        let sess = Session::connect(cfg, password, config_password, pw_source,
                                    ConnMode::Write, server_label.clone(), domain_label)?;
        self.sessions.push(sess);
        self.session_ui.push(SessionUi::default());
        self.rail_expanded.insert(server_label);
        self.rebuild_rail_rows();
        Ok(())
    }

    /// Build the plan for creating an OpenLDAP domain (derive the backend directory
    /// from a sibling via a `cn=config` bind), then either review-and-run it (if
    /// `provision_cmd` is set) or print the exact host commands to run.
    fn openldap_create_domain(&mut self, idx: usize, suffix: &str) {
        let suffix = suffix.trim().to_string();
        if !suffix.to_lowercase().starts_with("dc=") {
            self.status = Some((format!("expected a domain DN like dc=example,dc=org (got {suffix:?})"), true));
            return;
        }
        // A cn=config bind is needed to read a sibling's directory + add the backend.
        let cfg = self.sessions[idx].cfg.clone();
        let Some(config_bind) = cfg.server.config_bind_dn.clone() else {
            self.status = Some(("set config_bind_dn / config_password_cmd for this OpenLDAP server to create a domain".into(), true));
            return;
        };
        let mut ccfg = cfg.clone();
        ccfg.server.bind_dn = Some(config_bind);
        let base = match LdapClient::connect(&ccfg, self.sessions[idx].config_password.as_deref()) {
            Ok(mut c) => { let b = c.openldap_db_dir_base(); c.close().ok(); b }
            Err(e) => { self.status = Some((format!("cn=config connect failed: {e:#}"), true)); return; }
        };
        let be: String = suffix.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
        let dir = format!("{}/{be}", base.trim_end_matches('/'));
        let user = cfg.server.slapd_user.clone().unwrap_or_else(|| "openldap:openldap".to_string());
        let fs_script = format!("mkdir -p '{dir}' && chown {user} '{dir}'");

        match cfg.server.provision_cmd {
            Some(_) => {
                let prompt = format!(
                    "Create domain {suffix} on this OpenLDAP host.\n\
                     \n1. via provision_cmd, on the host:\n     {fs_script}\n\
                     \n2. census then, over LDAP:\n     add backend olcDatabase (suffix {suffix}, dir {dir})\n     add the apex + ou=users/ou=groups\n\
                     \nProceed?");
                self.pending_provision = Some(ProvisionPlan { session_idx: idx, suffix, dir, fs_script });
                self.overlay = Some(Overlay::Confirm(overlay::ConfirmDialog::review_provision(prompt)));
            }
            None => {
                self.status = Some((format!(
                    "no provision_cmd set. On the LDAP host run:  {fs_script}  — then set provision_cmd and retry (census will add the backend + skeleton)."), true));
            }
        }
    }

    /// Run the confirmed provisioning plan (create an OpenLDAP domain).
    fn execute_provision(&mut self) {
        let Some(plan) = self.pending_provision.take() else { return; };
        self.status = Some(match self.run_create_plan(&plan) {
            Ok(msg) => (msg, false),
            Err(e) => (format!("provision failed: {e:#}"), true),
        });
    }

    fn run_create_plan(&mut self, plan: &ProvisionPlan) -> anyhow::Result<String> {
        let idx = plan.session_idx;
        let cfg = self.sessions[idx].cfg.clone();
        let provision_cmd = cfg.server.provision_cmd.clone().context("no provision_cmd")?;
        let config_bind = cfg.server.config_bind_dn.clone().context("no config_bind_dn")?;
        let data_pw = self.sessions[idx].password.clone();
        let config_pw = self.sessions[idx].config_password.clone();
        // 1. Host filesystem: create the backend directory.
        crate::provision::run(&provision_cmd, &plan.fs_script).context("host provisioning")?;
        // 2. cn=config: add the backend (rootpw = the server's data admin password).
        let mut ccfg = cfg.clone();
        ccfg.server.bind_dn = Some(config_bind);
        let mut cclient = LdapClient::connect(&ccfg, config_pw.as_deref())?;
        let rc = cclient.create_openldap_backend(&plan.suffix, &plan.dir, data_pw.as_deref().unwrap_or(""));
        cclient.close().ok();
        rc?;
        // 3. Data: add the apex + skeleton as the new suffix's rootdn.
        let admin = format!("cn=admin,{}", plan.suffix);
        let mut dcfg = cfg.clone();
        dcfg.server.bind_dn = Some(admin.clone());
        dcfg.server.base_dn = plan.suffix.clone();
        let mut dclient = LdapClient::connect(&dcfg, data_pw.as_deref())?;
        let rc = dclient.add_domain_skeleton(&plan.suffix);
        dclient.close().ok();
        rc?;
        // 4. Register it in the rail.
        self.register_new_domain(idx, &plan.suffix, Some(admin))?;
        Ok(format!("created domain {}", plan.suffix))
    }

    /// Delete an OpenLDAP domain: remove its `olcDatabase` over cn=config (unmaps it),
    /// then `rm -rf` its directory on the host (best-effort), then drop the session.
    fn openldap_delete_domain(&mut self, idx: usize, suffix: &str) -> anyhow::Result<String> {
        let cfg = self.sessions[idx].cfg.clone();
        let config_bind = cfg.server.config_bind_dn.clone()
            .context("set config_bind_dn to delete a domain on this OpenLDAP server")?;
        let config_pw = self.sessions[idx].config_password.clone();
        let mut ccfg = cfg.clone();
        ccfg.server.bind_dn = Some(config_bind);
        let mut cclient = LdapClient::connect(&ccfg, config_pw.as_deref())?;
        let dir = cclient.delete_openldap_backend(suffix);
        cclient.close().ok();
        let dir = dir?;
        // Remove the directory on the host, if we can (else it just lingers, unserved).
        if let (Some(d), Some(cmd)) = (&dir, cfg.server.provision_cmd.as_deref()) {
            crate::provision::run(cmd, &format!("rm -rf '{d}'")).ok();
        }
        if let Some(t) = self.sessions.iter().position(|s| s.client.base_dn == suffix) {
            self.remove_session(t);
        }
        Ok(format!("deleted domain {suffix}"))
    }

    // ── migration (Phase 3) ──────────────────────────────────────────────────

    /// C (browse): copy the selected user from the focused connection (source) to the
    /// marked, writable connections (targets), rebasing its DN onto each target's base.
    /// Shows a review confirm first.
    /// C (user browse): copy the selected user to the marked connections.
    fn prepare_migration(&mut self) {
        let Some((dn, label)) = self.browse.snapshot.selected_user().map(|u| (u.dn.clone(), u.uid.clone())) else {
            self.status = Some(("no user selected to copy".into(), true));
            return;
        };
        self.prepare_migration_of(dn, label);
    }

    /// C (group browse): copy the cursored group to the marked connections. A posixGroup's
    /// `memberUid` values are bare uids (not DNs), so they carry over verbatim; only the
    /// group's own DN is rebased onto each target's base-DN.
    fn prepare_group_migration(&mut self) {
        let Some((dn, label)) = self.groups().get(self.groups_cur.cursor).map(|g| (g.dn.clone(), g.name.clone())) else {
            self.status = Some(("no group selected to copy".into(), true));
            return;
        };
        self.prepare_migration_of(dn, label);
    }

    /// Shared migration setup: gather the writable marked targets (excluding the source),
    /// build the review prompt, and stash a [`MigrationPlan`] for [`run_migration`].
    fn prepare_migration_of(&mut self, dn: String, label: String) {
        let source = self.focused;
        let mut targets: Vec<usize> = self.marked.iter().copied()
            .filter(|&t| t != source && t < self.sessions.len() && self.sessions[t].mode.can_write())
            .collect();
        targets.sort_unstable();
        if targets.is_empty() {
            self.status = Some(("mark writable target connection(s) with `m` in the rail (the focused one is the source)".into(), true));
            return;
        }
        let names: Vec<String> = targets.iter().map(|&t| self.sessions[t].label()).collect();
        let prompt = format!(
            "Copy {label}\nfrom {}\nto {} marked target(s):\n  {}\n\n(writes to write targets; previews on dry-run)\n\nProceed?",
            self.sessions[source].label(), targets.len(), names.join("\n  "));
        self.pending_migration = Some(MigrationPlan { source, targets, dn, label });
        self.overlay = Some(Overlay::Confirm(overlay::ConfirmDialog::review_migration(prompt)));
    }

    /// Run the confirmed migration: read the entry once from the source, then re-create
    /// it (DN rebased) at each target — writing on write targets, previewing on dry-run.
    fn run_migration(&mut self) {
        let Some(plan) = self.pending_migration.take() else { return; };
        let source_base = self.sessions[plan.source].client.base_dn.clone();
        let raw = match self.sessions[plan.source].client.read_entry_raw(&plan.dn) {
            Ok(r) => r,
            Err(e) => { self.status = Some((format!("read {} failed: {e:#}", plan.label), true)); return; }
        };
        let (mut written, mut previewed, mut failed) = (0u32, 0u32, 0u32);
        let mut first_err: Option<String> = None;
        for &t in &plan.targets {
            let target_base = self.sessions[t].client.base_dn.clone();
            let new_dn = rebase_dn(&plan.dn, &source_base, &target_base);
            match self.sessions[t].mode {
                ConnMode::DryRun => {
                    let dst = self.sessions[t].label();
                    let l = ldif::entry_ldif_raw(&new_dn, &raw);
                    self.journal.note(&format!("[dry-run] copy {} → {dst}", plan.label), &l);
                    previewed += 1;
                }
                ConnMode::Write => match self.sessions[t].client.add_raw(&new_dn, &raw) {
                    Ok(()) => written += 1,
                    Err(e) => { failed += 1; first_err.get_or_insert_with(|| format!("{e:#}")); }
                },
                ConnMode::ReadOnly => {}
            }
        }
        let mut msg = format!("copied {}: {written} written", plan.label);
        if previewed > 0 { msg += &format!(", {previewed} previewed (L to view)"); }
        if failed > 0 { msg += &format!(", {failed} failed — {}", first_err.unwrap_or_default()); }
        self.status = Some((msg, failed > 0));
    }

    /// D: confirm-and-delete the cursored domain (389-DS + Write; never the last one).
    fn rail_delete_domain(&mut self) {
        let Some(idx) = self.rail_rows.get(self.rail_cur.cursor).and_then(|r| r.session_idx) else {
            self.status = Some(("move the cursor onto a domain to delete it".into(), true));
            return;
        };
        if self.sessions.len() <= 1 {
            self.status = Some(("can't delete the only connection".into(), true));
            return;
        }
        let s = &self.sessions[idx];
        let brand = s.client.brand();
        if brand != Brand::Ds389 && brand != Brand::OpenLdap {
            self.status = Some((format!("deleting a domain isn't supported on {}", brand.label()), true));
            return;
        }
        if brand == Brand::OpenLdap && s.cfg.server.config_bind_dn.is_none() {
            self.status = Some(("set config_bind_dn for this OpenLDAP server to delete a domain".into(), true));
            return;
        }
        if s.mode != ConnMode::Write {
            self.status = Some(("set this connection to write (M) before deleting the domain".into(), true));
            return;
        }
        let suffix = s.client.base_dn.clone();
        self.overlay = Some(Overlay::Confirm(overlay::ConfirmDialog::delete_domain(
            format!("Delete domain {suffix} and unmap ALL its entries?"),
            suffix, idx,
        )));
    }

    /// Delete the domain session `idx` is connected to, then drop the session.
    fn do_delete_domain(&mut self, idx: usize) {
        if idx >= self.sessions.len() || self.sessions.len() <= 1 { return; }
        let suffix = self.sessions[idx].client.base_dn.clone();
        match self.sessions[idx].client.brand() {
            Brand::OpenLdap => {
                self.status = Some(match self.openldap_delete_domain(idx, &suffix) {
                    Ok(msg) => (msg, false),
                    Err(e) => (format!("delete failed: {e:#}"), true),
                });
                return;
            }
            _ => {
                if let Err(e) = self.sessions[idx].client.delete_domain(&suffix) {
                    self.status = Some((format!("delete failed: {e:#}"), true));
                    return;
                }
            }
        }
        self.remove_session(idx);
        self.status = Some((format!("deleted domain {suffix}"), false));
    }

    /// Remove session `target`: move focus off it first (so its live UI state isn't the
    /// App's), drop it and its stashed state (closing any browse worker), then reindex
    /// `focused` and the `marked` set for the shift.
    fn remove_session(&mut self, target: usize) {
        if target >= self.sessions.len() || self.sessions.len() <= 1 { return; }
        if self.focused == target {
            let fallback = if target == 0 { 1 } else { target - 1 };
            self.focus_session(fallback); // stashes target's live state; focused = fallback
        }
        self.sessions.remove(target);
        self.session_ui.remove(target);
        if self.focused > target { self.focused -= 1; }
        let old = std::mem::take(&mut self.marked);
        self.marked = old.into_iter()
            .filter(|&m| m != target)
            .map(|m| if m > target { m - 1 } else { m })
            .collect();
        self.rebuild_rail_rows();
        self.rail_cur.clamp(self.rail_rows.len());
    }

    /// s: open the schema browser for the cursored server (read-only; every brand).
    fn enter_schema(&mut self, session_idx: usize) {
        match self.sessions[session_idx].client.read_schema() {
            Ok(elems) => {
                self.schema_elems = elems;
                self.schema_cur = ListCursor::new();
                self.schema_focus = Pane::Left;
                self.schema_filter.clear();
                self.schema_filtering = false;
                let s = &self.sessions[session_idx];
                self.schema_title = format!("{} ({})", s.server_label, s.client.brand().label());
                self.schema_session = session_idx;
                self.schema_from = self.mode;
                self.mode = Mode::Schema;
                self.rail_focused = false; // hand key focus to the schema workspace
            }
            Err(e) => self.status = Some((format!("schema read failed: {e:#}"), true)),
        }
    }

    /// Indices of the schema elements matching the current filter (name or OID
    /// substring; all when the filter is empty).
    fn schema_matches(&self) -> Vec<usize> {
        let f = self.schema_filter.to_lowercase();
        self.schema_elems.iter().enumerate()
            .filter(|(_, e)| f.is_empty() || e.name.to_lowercase().contains(&f) || e.oid.contains(&f))
            .map(|(i, _)| i)
            .collect()
    }

    // ── schema browser accessors (for screens::schema) ──────────────────────
    pub fn schema_elems(&self) -> &[SchemaElem] { &self.schema_elems }
    pub fn schema_rows(&self) -> Vec<usize> { self.schema_matches() }
    pub fn schema_cursor(&self) -> &ListCursor { &self.schema_cur }
    pub fn schema_pane(&self) -> Pane { self.schema_focus }
    pub fn schema_filter(&self) -> &str { &self.schema_filter }
    pub fn schema_filtering(&self) -> bool { self.schema_filtering }
    pub fn schema_title(&self) -> &str { &self.schema_title }

    /// a / o: prompt to add an attributeType / objectClass. 389-DS uses the Directory
    /// Manager; OpenLDAP needs `config_bind_dn`/`config_password_cmd` in the config.
    fn open_add_schema(&mut self, kind: SchemaKind) {
        let idx = self.schema_session;
        if idx >= self.sessions.len() { return; }
        let s = &self.sessions[idx];
        let brand = s.client.brand();
        let supported = brand == Brand::Ds389
            || (brand == Brand::OpenLdap && s.cfg.server.config_bind_dn.is_some());
        if !supported {
            self.status = Some((if brand == Brand::OpenLdap {
                "set config_bind_dn / config_password_cmd for this OpenLDAP server to add schema".into()
            } else {
                format!("adding schema over LDAP isn't supported on {}", brand.label())
            }, true));
            return;
        }
        if s.mode != ConnMode::Write {
            self.status = Some(("set this connection to write (M in the rail) to add schema".into(), true));
            return;
        }
        self.overlay = Some(Overlay::Input(overlay::InputDialog::new_schema(idx, kind)));
    }

    /// Add a schema definition to `session_idx`'s server, then refresh the browser.
    /// OpenLDAP's schema lives under cn=config, so that path binds a separate config
    /// admin from the session's `config_*` creds.
    fn do_add_schema(&mut self, session_idx: usize, kind: SchemaKind, definition: &str) {
        if session_idx >= self.sessions.len() || self.sessions[session_idx].mode != ConnMode::Write {
            self.status = Some(("connection is read-only".into(), true));
            return;
        }
        let result = if self.sessions[session_idx].client.brand() == Brand::OpenLdap {
            self.openldap_add_schema(session_idx, kind, definition)
        } else {
            self.sessions[session_idx].client.add_schema(kind, definition)
        };
        match result {
            Ok(()) => {
                if self.mode == Mode::Schema && self.schema_session == session_idx {
                    if let Ok(elems) = self.sessions[session_idx].client.read_schema() {
                        self.schema_elems = elems;
                    }
                }
                let what = match kind { SchemaKind::Attribute => "attributeType", SchemaKind::ObjectClass => "objectClass" };
                self.status = Some((format!("added {what}"), false));
            }
            Err(e) => self.status = Some((format!("add schema failed: {e:#}"), true)),
        }
    }

    /// Add schema on an OpenLDAP server via a short-lived `cn=config`-admin connection
    /// (its schema lives under cn=config, which the data bind can't write).
    fn openldap_add_schema(&self, idx: usize, kind: SchemaKind, definition: &str) -> anyhow::Result<()> {
        let s = &self.sessions[idx];
        let config_bind = s.cfg.server.config_bind_dn.clone()
            .ok_or_else(|| anyhow::anyhow!("no config_bind_dn set for this OpenLDAP server"))?;
        let mut cfg = s.cfg.clone();
        cfg.server.bind_dn = Some(config_bind);
        let mut client = LdapClient::connect(&cfg, s.config_password.as_deref())?;
        let r = client.add_schema(kind, definition);
        client.close().ok();
        r
    }

    /// Rebuild the flattened rail: each server group (in first-seen order) followed by
    /// its domain leaves when expanded. Mirrors [`Self::rebuild_dit_rows`].
    fn rebuild_rail_rows(&mut self) {
        // Group session indices by server_label, preserving order of first appearance.
        let mut servers: Vec<(String, Vec<usize>)> = Vec::new();
        for (i, s) in self.sessions.iter().enumerate() {
            match servers.iter_mut().find(|(name, _)| name == &s.server_label) {
                Some((_, v)) => v.push(i),
                None => servers.push((s.server_label.clone(), vec![i])),
            }
        }
        let mut rows = Vec::new();
        let nservers = servers.len();
        for (si, (name, members)) in servers.iter().enumerate() {
            let server_last = si + 1 == nservers;
            let expanded = self.rail_expanded.contains(name);
            rows.push(RailRow {
                session_idx: None,
                label: name.clone(),
                ancestor_last: vec![],
                is_last: server_last,
                expanded: Some(expanded),
                is_server: true,
            });
            if expanded {
                let n = members.len();
                for (di, &sidx) in members.iter().enumerate() {
                    rows.push(RailRow {
                        session_idx: Some(sidx),
                        label: self.sessions[sidx].domain_label.clone(),
                        ancestor_last: vec![server_last],
                        is_last: di + 1 == n,
                        expanded: None,
                        is_server: false,
                    });
                }
            }
        }
        self.rail_rows = rows;
    }

    /// Whether the connections rail is shown (only with more than one connection).
    fn rail_visible(&self) -> bool { self.sessions.len() > 1 }

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

pub fn run(sessions: Vec<Session>) -> anyhow::Result<()> {
    let mut app = App::new(sessions);
    app.init_rail();
    if app.session().mode == ConnMode::DryRun {
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
        // Surface progress/completion from background domain backups.
        if let Some(status) = app.backups.poll() {
            app.status = Some(status);
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
            OverlayResult::CreateDomain { template_idx, suffix } => {
                app.overlay = None;
                app.do_create_domain(template_idx, &suffix);
            }
            OverlayResult::DeleteDomain { session_idx } => {
                app.overlay = None;
                app.do_delete_domain(session_idx);
            }
            OverlayResult::AddSchema { session_idx, kind, definition } => {
                app.overlay = None;
                app.do_add_schema(session_idx, kind, &definition);
            }
            OverlayResult::RunProvision => {
                app.overlay = None;
                app.execute_provision();
            }
            OverlayResult::RunMigration => {
                app.overlay = None;
                app.run_migration();
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
        Mode::Schema => {
            let n = app.schema_matches().len();
            if down { app.schema_cur.down(n); } else { app.schema_cur.up(); }
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

    // Connections rail (spans the full workspace height).
    let nrail = app.rail_rows.len();
    app.rail_cur.clamp(nrail);
    app.rail_cur.keep_in_view(nrail, vis);

    // Schema browser list (minus the filter line + separator).
    let nschema = app.schema_matches().len();
    let schema_vis = vis.saturating_sub(1).max(1);
    app.schema_cur.clamp(nschema);
    app.schema_cur.keep_in_view(nschema, schema_vis);

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

/// Rebase `dn` from `from_base` onto `to_base` (a case-insensitive suffix swap):
/// `uid=x,ou=users,dc=A` with bases `dc=A`→`dc=B` becomes `uid=x,ou=users,dc=B`. If
/// `dn` isn't under `from_base`, it's returned unchanged.
fn rebase_dn(dn: &str, from_base: &str, to_base: &str) -> String {
    if dn.len() >= from_base.len()
        && dn[dn.len() - from_base.len()..].eq_ignore_ascii_case(from_base)
    {
        format!("{}{}", &dn[..dn.len() - from_base.len()], to_base)
    } else {
        dn.to_string()
    }
}

/// Key handling while the connections rail holds focus: navigate the server→domain
/// tree, focus a domain, mark connections, and cycle a connection's mode.
fn handle_rail_key(app: &mut App, key: KeyCode) -> anyhow::Result<bool> {
    use KeyCode::*;
    match key {
        Char('q') => return Ok(true),
        Esc => app.rail_focused = false,
        Up   | Char('k') => app.rail_cur.up(),
        Down | Char('j') => app.rail_cur.down(app.rail_rows.len()),
        Char('l') | Right | Enter | Char(' ') => app.rail_activate(),
        Char('h') | Left => app.rail_collapse(),
        Char('m') => app.rail_toggle_mark(),
        Char('M') => app.rail_cycle_mode(),
        Char('b') => app.rail_backup(),
        Char('N') => app.rail_new_domain(),
        Char('D') => app.rail_delete_domain(),
        Char('s') => { if let Some(idx) = app.rail_template_session() { app.enter_schema(idx); } }
        _ => {}
    }
    Ok(false)
}

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

    // A pending mode change (enabling live writes) waits for y/n confirmation.
    if let Some((idx, newmode)) = app.pending_mode.take() {
        if matches!(key, Char('y') | Char('Y') | Enter) {
            app.sessions[idx].mode = newmode;
            app.status = Some((format!("{} → {}", app.sessions[idx].label(), newmode.tag()), false));
        } else {
            app.status = Some(("mode change cancelled".into(), false));
        }
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
            OverlayResult::CreateDomain { template_idx, suffix } => {
                app.overlay = None;
                app.do_create_domain(template_idx, &suffix);
            }
            OverlayResult::DeleteDomain { session_idx } => {
                app.overlay = None;
                app.do_delete_domain(session_idx);
            }
            OverlayResult::AddSchema { session_idx, kind, definition } => {
                app.overlay = None;
                app.do_add_schema(session_idx, kind, &definition);
            }
            OverlayResult::RunProvision => {
                app.overlay = None;
                app.execute_provision();
            }
            OverlayResult::RunMigration => {
                app.overlay = None;
                app.run_migration();
            }
        }
        return Ok(false);
    }

    // `?` opens the manual from any screen.
    if key == Char('?') {
        app.overlay = Some(Overlay::Help(overlay::HelpView::new()));
        return Ok(false);
    }

    // Backtick toggles key focus between the connections rail and the workspace; while
    // the rail holds focus it consumes navigation (so `jk`/`Enter`/`m`/`M` drive it).
    if key == Char('`') && app.rail_visible() {
        app.rail_focused = !app.rail_focused;
        return Ok(false);
    }
    if app.rail_focused {
        return handle_rail_key(app, key);
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
    if key == Char('/') && app.mode != Mode::Search && app.mode != Mode::Schema {
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
            (_, Char('C')) => app.prepare_migration(),
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
            (_, Char('C')) => app.prepare_group_migration(),
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
                    app.status = Some(("Read-only connection — enable writes with --write or in the rail (` then M)".into(), true));
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

        Mode::Schema => {
            let n = app.schema_matches().len();
            if app.schema_filtering {
                // Editing the filter: type to narrow, Enter/Esc to stop editing.
                match key {
                    Esc | Enter => app.schema_filtering = false,
                    Backspace => { app.schema_filter.pop(); app.schema_cur.cursor = 0; }
                    Char(c) if !mods.contains(KeyModifiers::CONTROL) => {
                        app.schema_filter.push(c); app.schema_cur.cursor = 0;
                    }
                    _ => {}
                }
            } else {
                match (app.schema_focus, key) {
                    (_, Char('q')) => return Ok(true),
                    (_, Esc) => app.mode = app.schema_from,
                    (_, Char('/')) => app.schema_filtering = true,
                    (_, Char('a')) => app.open_add_schema(SchemaKind::Attribute),
                    (_, Char('o')) => app.open_add_schema(SchemaKind::ObjectClass),
                    (_, Tab) | (_, BackTab) =>
                        app.schema_focus = if app.schema_focus == Pane::Left { Pane::Right } else { Pane::Left },
                    (Pane::Left, Up   | Char('k')) => app.schema_cur.up(),
                    (Pane::Left, Down | Char('j')) => app.schema_cur.down(n),
                    (Pane::Left, PageUp)   => app.schema_cur.page(-10, n),
                    (Pane::Left, PageDown) => app.schema_cur.page(10, n),
                    _ => {}
                }
            }
        }
    }
    Ok(false)
}

/// Open a typed-DN delete confirmation for the DIT-selected entry.
fn open_dit_delete(app: &mut App) {
    if !app.can_write_ui() {
        app.status = Some(("Read-only connection — enable writes with --write or in the rail (` then M)".into(), true));
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
        app.status = Some(("Read-only connection — enable writes with --write or in the rail (` then M)".into(), true));
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
        app.status = Some(("Read-only connection — enable writes with --write or in the rail (` then M)".into(), true));
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
        app.status = Some(("Read-only connection — enable writes with --write or in the rail (` then M)".into(), true));
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
        app.status = Some(("Read-only connection — enable writes with --write or in the rail (` then M)".into(), true));
        return;
    }
    let Some(user) = app.detail() else { return; };
    let editor = overlay::KeyEditor::new(user.dn.clone(), user.ssh_keys.clone());
    app.overlay = Some(Overlay::Keys(editor));
}

/// Open the set-password dialog for the cursored user.
fn open_passwd(app: &mut App) {
    if !app.can_write_ui() {
        app.status = Some(("Read-only connection — enable writes with --write or in the rail (` then M)".into(), true));
        return;
    }
    let Some(user) = app.detail() else { return; };
    let dlg = overlay::PasswdDialog::new(user.dn.clone(), user.uid.clone());
    app.overlay = Some(Overlay::Passwd(dlg));
}

/// Open the new-user form, seeded with the next free uidNumber.
fn open_new_user(app: &mut App) {
    if !app.can_write_ui() {
        app.status = Some(("Read-only connection — enable writes with --write or in the rail (` then M)".into(), true));
        return;
    }
    let suggested = app.session_mut().client.next_uid_number().unwrap_or(10000);
    app.overlay = Some(Overlay::NewUser(overlay::NewUserForm::new(suggested)));
}

/// Open a typed-DN delete confirmation for the user under the list cursor.
fn open_delete_user(app: &mut App) {
    if !app.can_write_ui() {
        app.status = Some(("Read-only connection — enable writes with --write or in the rail (` then M)".into(), true));
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
        app.status = Some(("Read-only connection — enable writes with --write or in the rail (` then M)".into(), true));
        return;
    }
    let suggested = app.session_mut().client.next_gid_number().unwrap_or(10000);
    app.overlay = Some(Overlay::NewGroup(overlay::NewGroupForm::new(suggested)));
}

/// Open a typed-DN delete confirmation for the group under the cursor.
fn open_delete_group(app: &mut App) {
    if !app.can_write_ui() {
        app.status = Some(("Read-only connection — enable writes with --write or in the rail (` then M)".into(), true));
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
        app.status = Some(("Read-only connection — enable writes with --write or in the rail (` then M)".into(), true));
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
        app.status = Some(("Read-only connection — enable writes with --write or in the rail (` then M)".into(), true));
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
        app.status = Some(("Read-only connection — enable writes with --write or in the rail (` then M)".into(), true));
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
        app.status = Some(("Read-only connection — enable writes with --write or in the rail (` then M)".into(), true));
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
    if app.session().mode == ConnMode::DryRun {
        let base_dn = app.session().client.base_dn.clone();
        let schema  = app.session().client.schema().clone();
        let ldif    = ldif::action_ldif(&action, &base_dn, &schema);
        app.journal.note(&format!("[dry-run] {}", describe_action(&action)), &ldif);
        app.status = Some((format!("[dry-run] {}", describe_action(&action)), false));
        return Ok(());
    }
    if !app.can_write_ui() {
        app.status = Some(("Read-only connection — enable writes with --write or in the rail (` then M)".into(), true));
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
    if app.session().mode == ConnMode::DryRun {
        app.status = Some(("dry-run: nothing is actually written, so nothing to undo".into(), false));
        return Ok(());
    }
    if !app.can_write_ui() {
        app.status = Some(("Read-only connection — enable writes with --write or in the rail (` then M)".into(), true));
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

    // With more than one connection (and room), a left rail lists them; the focused
    // connection's workspace fills the rest. One connection → today's full-width view.
    let rail_w: u16 = if app.rail_visible() && main.width >= 60 { 26 } else { 0 };
    let (rail_rect, workspace) = if rail_w > 0 {
        (Some(Rect::new(main.x, main.y, rail_w, main.height)),
         Rect::new(main.x + rail_w, main.y, main.width - rail_w, main.height))
    } else {
        (None, main)
    };

    match app.mode {
        Mode::Browse      => screens::users::render(app, buf, workspace, app.browse_focus),
        Mode::GroupSelect => screens::groups::render_select(app, buf, workspace),
        Mode::Membership  => screens::groups::render_membership(app, buf, workspace),
        Mode::Dit         => screens::dit::render(app, buf, workspace),
        Mode::Search      => screens::search::render(app, buf, workspace),
        Mode::Schema      => screens::schema::render(app, buf, workspace),
    }
    if let Some(pa) = preview {
        screens::preview::render(app, buf, pa);
    }
    if let Some(rr) = rail_rect {
        screens::rail::render(app, buf, rr);
    }

    // Connection/password "gap" in the top border of the workspace frame (content
    // pass): drawn over the frame the screen just laid down, and excluded from the glow.
    let gap = super::topgap::draw_top_gap(buf, workspace, &app.conn_info().summary());
    // Travelling glow on the workspace frame, under any modal overlay — skipping the gap.
    if app.anim_on {
        glow::edge_glow(buf, workspace, app.anim_start.elapsed().as_secs_f32(), gap.as_slice());
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
    fn rebase_dn_swaps_the_base_suffix() {
        // The RDN chain above the base is preserved; only the base suffix changes.
        assert_eq!(
            rebase_dn("uid=ada,ou=users,dc=alpha,dc=test", "dc=alpha,dc=test", "dc=bravo,dc=test"),
            "uid=ada,ou=users,dc=bravo,dc=test"
        );
        // The source base match is case-insensitive (DNs aren't case-sensitive in the suffix).
        assert_eq!(
            rebase_dn("uid=ada,ou=users,DC=Alpha,DC=Test", "dc=alpha,dc=test", "dc=bravo,dc=test"),
            "uid=ada,ou=users,dc=bravo,dc=test"
        );
        // A DN not under the source base is left untouched.
        assert_eq!(
            rebase_dn("uid=ada,dc=other", "dc=alpha,dc=test", "dc=bravo,dc=test"),
            "uid=ada,dc=other"
        );
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
