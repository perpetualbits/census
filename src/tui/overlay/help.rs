//! In-app manual: a scrollable help overlay opened with `?`.

use mullion::{Buffer, KeyCode, KeyModifiers, Rect};

use crate::tui::draw::btxt;
use crate::tui::theme::*;

use super::{modal_frame, OverlayResult};

/// The manual text. Lines beginning with `#` are section headers; blank lines
/// are spacers; everything else is body text.
const MANUAL: &[&str] = &[
    "# census — LDAP user & group administration",
    "Browse users and groups, inspect and edit entries, manage SSH keys,",
    "passwords and group membership. Read-only unless started with --write.",
    "",
    "# Getting around",
    "  Tab            switch between the user list and the detail pane",
    "  /              search users & groups (name, uid, group, uid/gid number)",
    "  q              quit",
    "  Esc            step focus back, then quit",
    "  Ctrl+G         toggle the travelling border glow",
    "  ?              open / close this manual",
    "",
    "# Connections rail  (multiple directories)",
    "  Load several servers/domains at once by dropping one .toml per connection in",
    "  ~/.config/census/conf.d/ (a server file may list extra [[domain]] base-DNs).",
    "  With more than one connection a left rail appears, grouping domains by server.",
    "  `              toggle focus between the rail and the workspace",
    "  j / k          move within the rail;  l / Enter expand a server, h collapse",
    "  Enter / Space  focus a domain — its users/groups fill the workspace",
    "  m              mark / unmark a connection (▣) for set operations",
    "  M              cycle a connection's mode: read-only ○ → write ● → dry-run ✎",
    "                 (enabling writes asks y/n first). ◀ marks the focused domain.",
    "  b              back up the cursored domain to an LDIF file (in the working dir)",
    "",
    "# Search  (/)",
    "  Type to filter users and groups live; matches come from first/last name,",
    "  account name (uid), group name, and uidNumber / gidNumber.",
    "  ↑ / ↓          move through the matches",
    "  Enter          jump to the selected user or group",
    "  Esc            cancel and return to the previous screen",
    "",
    "# User list  (Browse, left pane)",
    "  j / k, arrows  move the cursor",
    "  PgUp / PgDn    jump ten rows",
    "  n              create a new user",
    "  D              delete the selected user (type its DN to confirm)",
    "  g              open the group picker",
    "",
    "# Detail pane  (Browse, right pane — press Tab)",
    "  j / k          move the editable-attribute cursor",
    "  e              edit the selected attribute",
    "  E              big-edit the attribute (multi-line; Enter=newline, Ctrl+S=save)",
    "  K              manage SSH public keys",
    "  p              set / reset the password",
    "  Values in any script render right-to-left/mixed automatically.",
    "",
    "# SSH key manager  (K)",
    "  a              add a key (paste a full OpenSSH key line)",
    "  d              delete the selected key",
    "  s              save the key set",
    "  Esc            cancel",
    "  The ldapPublicKey object class is added automatically when needed.",
    "",
    "# Groups  (g)  — list (left) + detail pane (right), like the user browse",
    "  j / k          move the cursor (list) / editable attribute (detail)",
    "  Tab            switch between the group list and the detail pane",
    "  Enter          manage the group's membership (from the list)",
    "  n              create a new group",
    "  D              delete the selected group (type its DN to confirm)",
    "  a              remove the group's extra cn (an 'aka' alias/name)",
    "  Duplicate names/gidNumbers are flagged in amber with a ⚠ marker.",
    "",
    "# Group detail pane  (Tab from the group list)",
    "  j / k          move the editable-attribute cursor",
    "  e              edit the selected attribute (gidNumber, description;",
    "                 description can be added when absent)",
    "  r              rename the group — changes its cn/RDN (LDAP modrdn)",
    "  Members are shown here but managed via the membership editor (Enter).",
    "",
    "# Membership editor",
    "  Tab            switch between all-users and members",
    "  Enter          add (from all-users) / remove (from members)",
    "  Removing a member asks for confirmation first.",
    "",
    "# DIT tree browser  (t from the user list)",
    "  A tree of the whole directory (left) + the selected entry's attributes (right).",
    "  j / k          move the tree cursor",
    "  l / Enter      expand (loads children on first open) / collapse",
    "  h              collapse",
    "  Tab            switch between the tree and the entry pane",
    "  D              delete the selected entry (type its DN to confirm; undoable)",
    "  e / E          edit an attribute of the selected entry (Tab to the entry pane)",
    "  Esc            back to the user list",
    "",
    "# Writing changes",
    "  census is read-only by default; start it with --write to modify.",
    "  Destructive actions confirm first; deleting an entry requires you",
    "  to type its full DN.",
    "",
    "# Rollback & LDIF inspection",
    "  u              undo the last write (reversible ops; write mode)",
    "  L              toggle the LDIF change-preview tile (any screen)",
    "  Every write is also appended, in LDIF, to a session journal under",
    "  $XDG_STATE_HOME/census (default ~/.local/state/census/journal-*.ldif),",
    "  replayable with ldapmodify. Password changes are logged but not undoable.",
    "",
    "# Passwords",
    "  password_scheme = \"exop\"   server-side RFC 3062 modify (default)",
    "  password_scheme = \"crypt\"  client-side {CRYPT}$6$ (SHA-512)",
    "",
    "# Connection",
    "  Config:   ~/.config/census/config.toml   (kept out of version control)",
    "  Password: password_cmd (e.g. rbw), $CENSUS_BIND_PASSWORD, or a prompt",
    "  census --ping   check connectivity and print user/group counts",
];

pub struct HelpView {
    scroll: usize,
}

impl HelpView {
    pub fn new() -> Self { Self { scroll: 0 } }

    pub fn handle_key(&mut self, key: KeyCode, _mods: KeyModifiers) -> OverlayResult {
        use KeyCode::*;
        let last = MANUAL.len().saturating_sub(1);
        match key {
            Esc | Char('q') | Char('?') => return OverlayResult::Cancel,
            Up   | Char('k') => self.scroll = self.scroll.saturating_sub(1),
            Down | Char('j') => self.scroll = (self.scroll + 1).min(last),
            PageUp           => self.scroll = self.scroll.saturating_sub(10),
            PageDown         => self.scroll = (self.scroll + 10).min(last),
            Home | Char('g') => self.scroll = 0,
            End  | Char('G') => self.scroll = last,
            _ => {}
        }
        OverlayResult::Stay
    }

    pub fn render(&self, buf: &mut Buffer, area: Rect) {
        let w = area.width.saturating_sub(6).clamp(40, 78);
        let h = area.height.saturating_sub(4).clamp(8, 40);
        let rect = modal_frame(buf, area, w, h);
        btxt(buf, rect.x + 2, rect.y, "  census — manual  ", s_title());
        btxt(buf, rect.x + 2, rect.y + rect.height - 1,
             " jk/PgUp/PgDn:scroll  g/G:top/bottom  ?/Esc:close ", s_dim());

        let body_x = rect.x + 2;
        let body_w = rect.width.saturating_sub(4) as usize;
        let body_h = rect.height.saturating_sub(2) as usize;

        for (row, line) in MANUAL.iter().skip(self.scroll).take(body_h).enumerate() {
            let y = rect.y + 1 + row as u16;
            if let Some(header) = line.strip_prefix("# ") {
                btxt(buf, body_x, y, &clip(header, body_w), s_head());
            } else {
                btxt(buf, body_x, y, &clip(line, body_w), s_normal());
            }
        }

        // Scroll indicator.
        if MANUAL.len() > body_h {
            let pos = format!(" {}/{} ", (self.scroll + body_h).min(MANUAL.len()), MANUAL.len());
            let px = rect.x + rect.width.saturating_sub(1 + pos.len() as u16);
            btxt(buf, px, rect.y, &pos, s_dim());
        }
    }
}

fn clip(s: &str, w: usize) -> String {
    if s.chars().count() <= w { s.to_string() }
    else { s.chars().take(w.saturating_sub(1)).collect::<String>() + "…" }
}
