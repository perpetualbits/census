//! Modal overlays and the contract between them and the app.
//!
//! An overlay, while present, consumes every keystroke. On each key it returns
//! an [`OverlayResult`]: keep going, cancel, or commit a decoupled [`Action`]
//! that the app's single `perform()` chokepoint executes (after the `--write`
//! gate). Overlays never touch the `LdapClient` themselves.

pub mod confirm;
pub mod help;
pub mod input;
pub mod keys;
pub mod newgroup;
pub mod newuser;
pub mod passwd;
pub mod textarea;

use mullion::{
    draw_panel,
    float::{FloatChild, FloatLayer, FloatRect},
    Buffer, KeyCode, KeyModifiers, Panel, Rect,
};

use crate::tui::theme::{box_style, s_normal};

pub use confirm::ConfirmDialog;
pub use help::HelpView;
pub use input::InputDialog;
pub use keys::KeyEditor;
pub use newgroup::NewGroupForm;
pub use newuser::NewUserForm;
pub use passwd::PasswdDialog;
pub use textarea::TextAreaDialog;

use crate::ldap::client::NewUserSpec;

/// A write the user has requested. The app gates and executes these centrally.
#[derive(Clone)]
pub enum Action {
    /// Replace an attribute's values (empty = clear the attribute).
    SetAttr { dn: String, attr: String, values: Vec<String> },
    /// Replace the full set of SSH public keys (empty = clear).
    SetKeys { dn: String, keys: Vec<String> },
    /// Add a user to a group's membership.
    AddMember { group_dn: String, uid: String, group: String },
    /// Remove a user from a group's membership.
    DelMember { group_dn: String, uid: String, group: String },
    /// Set a user's password (plaintext; the client hashes per config).
    SetPasswd { dn: String, plaintext: String },
    /// Create a new user entry.
    CreateUser(NewUserSpec),
    /// Delete a user entry by DN (`label` is shown in status messages).
    DeleteEntry { dn: String, label: String },
    /// Create a new posixGroup.
    CreateGroup { name: String, gid_number: u32 },
    /// Delete a group entry by DN.
    DeleteGroup { dn: String, name: String },
    /// Rename a group: change its `cn` RDN (an LDAP modrdn). `old_name` is kept so
    /// the change is describable and reversible.
    RenameGroup { dn: String, new_cn: String, old_name: String },
    /// Remove one non-RDN `cn` value (an alias) from a group entry.
    RemoveAlias { dn: String, alias: String, group: String },
    /// Add a `cn` value to a group entry (the inverse of [`Action::RemoveAlias`]).
    AddAlias { dn: String, alias: String, group: String },
    /// Re-create a previously-captured entry verbatim — the inverse of a delete,
    /// used by the undo stack. Values are raw bytes so binary attributes (e.g.
    /// `jpegPhoto`) round-trip. Never produced by an overlay; only by rollback.
    RestoreEntry { dn: String, attrs: Vec<(String, Vec<Vec<u8>>)>, label: String },
}

/// What a modal asks the app to do after a keystroke.
pub enum OverlayResult {
    /// Keep the modal open and redraw.
    Stay,
    /// Close the modal without doing anything.
    Cancel,
    /// Close the modal and perform this action.
    Commit(Action),
    /// Close the modal and create a new domain `suffix` on the server of session
    /// `template_idx` (handled outside the `Action`/undo machinery — it manages
    /// sessions, not entries).
    CreateDomain { template_idx: usize, suffix: String },
    /// Close the modal and delete the domain that session `session_idx` is connected
    /// to, then drop that session from the rail.
    DeleteDomain { session_idx: usize },
}

/// The set of modal dialogs. One is active at a time via `App::overlay`.
pub enum Overlay {
    Input(InputDialog),
    Keys(KeyEditor),
    Confirm(ConfirmDialog),
    Passwd(PasswdDialog),
    NewUser(NewUserForm),
    NewGroup(NewGroupForm),
    TextArea(TextAreaDialog),
    Help(HelpView),
}

impl Overlay {
    pub fn handle_key(&mut self, key: KeyCode, mods: KeyModifiers) -> OverlayResult {
        match self {
            Overlay::Input(d)    => d.handle_key(key, mods),
            Overlay::Keys(d)     => d.handle_key(key, mods),
            Overlay::Confirm(d)  => d.handle_key(key, mods),
            Overlay::Passwd(d)   => d.handle_key(key, mods),
            Overlay::NewUser(d)  => d.handle_key(key, mods),
            Overlay::NewGroup(d) => d.handle_key(key, mods),
            Overlay::TextArea(d) => d.handle_key(key, mods),
            Overlay::Help(d)     => d.handle_key(key, mods),
        }
    }

    /// Deliver a bracketed paste to the active field. Overlays without a text field
    /// ignore it (returning `Stay`), so a paste is never interpreted as commands.
    pub fn handle_paste(&mut self, text: &str) -> OverlayResult {
        match self {
            Overlay::Input(d)    => d.handle_paste(text),
            Overlay::Keys(d)     => d.handle_paste(text),
            Overlay::Confirm(d)  => d.handle_paste(text),
            Overlay::Passwd(d)   => d.handle_paste(text),
            Overlay::NewUser(d)  => d.handle_paste(text),
            Overlay::NewGroup(d) => d.handle_paste(text),
            Overlay::TextArea(d) => d.handle_paste(text),
            Overlay::Help(_)     => OverlayResult::Stay,
        }
    }

    pub fn render(&self, buf: &mut Buffer, area: Rect) {
        match self {
            Overlay::Input(d)    => d.render(buf, area),
            Overlay::Keys(d)     => d.render(buf, area),
            Overlay::Confirm(d)  => d.render(buf, area),
            Overlay::Passwd(d)   => d.render(buf, area),
            Overlay::NewUser(d)  => d.render(buf, area),
            Overlay::NewGroup(d) => d.render(buf, area),
            Overlay::TextArea(d) => d.render(buf, area),
            Overlay::Help(d)     => d.render(buf, area),
        }
    }
}

/// Insert pasted `text` into a caller-owned field at byte offset `*cursor`, advancing
/// it. Control characters are dropped (newlines too, unless `multiline`) — this is what
/// makes a paste land as *data* rather than as a stream of command keystrokes.
pub fn paste_into(value: &mut String, cursor: &mut usize, text: &str, multiline: bool) {
    let clean: String = text
        .chars()
        .filter(|&c| (multiline && c == '\n') || !c.is_control())
        .collect();
    if clean.is_empty() { return; }
    let at = (*cursor).min(value.len());
    value.insert_str(at, &clean);
    *cursor = at + clean.len();
}

/// Census-styled modal chrome: centre a `w`×`h` box, clear its interior and draw the
/// rounded frame in one pass via [`draw_panel`]. Returns the outer rect — the caller
/// draws its own title/footer over that border and content inside it.
pub fn modal_frame(buf: &mut Buffer, area: Rect, w: u16, h: u16) -> Rect {
    let rect = center(area, w, h);
    draw_panel(buf, rect, &Panel::new(box_style()).fill(s_normal()));
    rect
}

/// A rect of size `w`×`h` centred within `area` (clamped to fit).
///
/// Placement goes through mullion's float layer: the modal is declared as a
/// single parent-local [`FloatRect`] and `solve` translates it to absolute
/// coordinates and clips it to `area`.
pub fn center(area: Rect, w: u16, h: u16) -> Rect {
    const MODAL: u64 = 0;
    let w = w.min(area.width);
    let h = h.min(area.height);
    let place = FloatRect::new((area.width - w) / 2, (area.height - h) / 2, w, h);
    FloatLayer::new()
        .with_child(FloatChild::new(MODAL, place))
        .solve(area)
        .first()
        .map(|&(_, r)| r)
        .unwrap_or(area)
}
