//! Modal overlays and the contract between them and the app.
//!
//! An overlay, while present, consumes every keystroke. On each key it returns
//! an [`OverlayResult`]: keep going, cancel, or commit a decoupled [`Action`]
//! that the app's single `perform()` chokepoint executes (after the `--write`
//! gate). Overlays never touch the `LdapClient` themselves.

pub mod confirm;
pub mod input;
pub mod keys;
pub mod newuser;
pub mod passwd;

use mullion::{Buffer, KeyCode, KeyModifiers, Rect};

pub use confirm::ConfirmDialog;
pub use input::InputDialog;
pub use keys::KeyEditor;
pub use newuser::NewUserForm;
pub use passwd::PasswdDialog;

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
    /// Delete an entry by DN (`label` is shown in status messages).
    DeleteEntry { dn: String, label: String },
}

/// What a modal asks the app to do after a keystroke.
pub enum OverlayResult {
    /// Keep the modal open and redraw.
    Stay,
    /// Close the modal without doing anything.
    Cancel,
    /// Close the modal and perform this action.
    Commit(Action),
}

/// The set of modal dialogs. One is active at a time via `App::overlay`.
pub enum Overlay {
    Input(InputDialog),
    Keys(KeyEditor),
    Confirm(ConfirmDialog),
    Passwd(PasswdDialog),
    NewUser(NewUserForm),
}

impl Overlay {
    pub fn handle_key(&mut self, key: KeyCode, mods: KeyModifiers) -> OverlayResult {
        match self {
            Overlay::Input(d)   => d.handle_key(key, mods),
            Overlay::Keys(d)    => d.handle_key(key, mods),
            Overlay::Confirm(d) => d.handle_key(key, mods),
            Overlay::Passwd(d)  => d.handle_key(key, mods),
            Overlay::NewUser(d) => d.handle_key(key, mods),
        }
    }

    pub fn render(&self, buf: &mut Buffer, area: Rect) {
        match self {
            Overlay::Input(d)   => d.render(buf, area),
            Overlay::Keys(d)    => d.render(buf, area),
            Overlay::Confirm(d) => d.render(buf, area),
            Overlay::Passwd(d)  => d.render(buf, area),
            Overlay::NewUser(d) => d.render(buf, area),
        }
    }
}

/// A rect of size `w`×`h` centred within `area` (clamped to fit).
pub fn center(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h)
}
