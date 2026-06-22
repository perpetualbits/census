//! In-app manual: a scrollable help overlay opened with `?`.

use mullion::{border::Borders, Buffer, KeyCode, KeyModifiers, Rect};

use crate::tui::draw::btxt;
use crate::tui::theme::*;

use super::{center, OverlayResult};

/// The manual text. Lines beginning with `#` are section headers; blank lines
/// are spacers; everything else is body text.
const MANUAL: &[&str] = &[
    "# census — LDAP user & group administration",
    "Browse users and groups, inspect and edit entries, manage SSH keys,",
    "passwords and group membership. Read-only unless started with --write.",
    "",
    "# Getting around",
    "  Tab            switch between the user list and the detail pane",
    "  q              quit",
    "  Esc            step focus back, then quit",
    "  Ctrl+G         toggle the travelling border glow",
    "  ?              open / close this manual",
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
    "  K              manage SSH public keys",
    "  p              set / reset the password",
    "",
    "# SSH key manager  (K)",
    "  a              add a key (paste a full OpenSSH key line)",
    "  d              delete the selected key",
    "  s              save the key set",
    "  Esc            cancel",
    "  The ldapPublicKey object class is added automatically when needed.",
    "",
    "# Groups  (g)",
    "  j / k          move the cursor",
    "  Enter          manage the group's membership",
    "  n              create a new group",
    "  D              delete the selected group (type its DN to confirm)",
    "",
    "# Membership editor",
    "  Tab            switch between all-users and members",
    "  Enter          add (from all-users) / remove (from members)",
    "  Removing a member asks for confirmation first.",
    "",
    "# Writing changes",
    "  census is read-only by default; start it with --write to modify.",
    "  Destructive actions confirm first; deleting an entry requires you",
    "  to type its full DN.",
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
        let rect = center(area, w, h);

        for y in rect.y..rect.y + rect.height {
            for x in rect.x..rect.x + rect.width {
                buf.set_string(x, y, " ", s_normal());
            }
        }
        mullion::border::draw_box(buf, rect, Borders::ALL, &box_style());
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
