//! New-user form modal: a vertical list of fields that builds a [`NewUserSpec`].
//!
//! `Tab`/arrows move between fields, typing edits the focused field, `Enter`
//! submits. On a validation error the form stays open with a red message.

use mullion::{line_edit, render_field, Buffer, FieldRender, KeyCode, KeyModifiers, Rect};

use crate::ldap::client::NewUserSpec;
use crate::tui::draw::btxt;
use crate::tui::theme::*;

use super::{modal_frame, Action, OverlayResult};

#[derive(Clone, Copy, PartialEq)]
enum Key { Uid, Cn, Sn, Given, UidNumber, GidNumber, Home, Shell, Password }

struct Field {
    key: Key,
    label: &'static str,
    value: String,
    cursor: usize, // byte index into `value`
    masked: bool,
}

impl Field {
    fn new(key: Key, label: &'static str, value: &str, masked: bool) -> Self {
        let value = value.to_string();
        Self { key, label, cursor: value.len(), value, masked }
    }
}

pub struct NewUserForm {
    fields: Vec<Field>,
    cursor: usize,
    error: Option<String>,
}

impl NewUserForm {
    /// Seed the form; `uid_number` pre-fills the uid/gid number fields.
    pub fn new(uid_number: u32) -> Self {
        let n = uid_number.to_string();
        Self {
            fields: vec![
                Field::new(Key::Uid,       "uid",        "",          false),
                Field::new(Key::Cn,        "cn",         "",          false),
                Field::new(Key::Sn,        "sn",         "",          false),
                Field::new(Key::Given,     "givenName",  "",          false),
                Field::new(Key::UidNumber, "uidNumber",  &n,          false),
                Field::new(Key::GidNumber, "gidNumber",  &n,          false),
                Field::new(Key::Home,      "home",       "",          false),
                Field::new(Key::Shell,     "shell",      "/bin/bash", false),
                Field::new(Key::Password,  "password",   "",          true),
            ],
            cursor: 0,
            error: None,
        }
    }

    fn get(&self, key: Key) -> String {
        self.fields.iter().find(|f| f.key == key).map(|f| f.value.clone()).unwrap_or_default()
    }

    fn build(&self) -> Result<NewUserSpec, String> {
        let uid = self.get(Key::Uid);
        let cn  = self.get(Key::Cn);
        let sn  = self.get(Key::Sn);
        if uid.is_empty() { return Err("uid is required".into()); }
        if cn.is_empty()  { return Err("cn is required".into()); }
        if sn.is_empty()  { return Err("sn is required".into()); }

        let uid_number = self.get(Key::UidNumber).parse::<u32>()
            .map_err(|_| "uidNumber must be a number".to_string())?;
        let gid_number = self.get(Key::GidNumber).parse::<u32>()
            .map_err(|_| "gidNumber must be a number".to_string())?;

        let given = self.get(Key::Given);
        let home = {
            let h = self.get(Key::Home);
            if h.is_empty() { format!("/home/{uid}") } else { h }
        };
        let shell = {
            let s = self.get(Key::Shell);
            if s.is_empty() { "/bin/bash".to_string() } else { s }
        };
        let password = {
            let p = self.get(Key::Password);
            if p.is_empty() { None } else { Some(p) }
        };

        Ok(NewUserSpec {
            uid, cn, sn,
            given_name: if given.is_empty() { None } else { Some(given) },
            uid_number, gid_number, home, shell, password,
        })
    }

    pub fn handle_key(&mut self, key: KeyCode, _mods: KeyModifiers) -> OverlayResult {
        use KeyCode::*;
        match key {
            Esc => OverlayResult::Cancel,
            Tab | Down => { self.cursor = (self.cursor + 1) % self.fields.len(); OverlayResult::Stay }
            BackTab | Up => {
                self.cursor = (self.cursor + self.fields.len() - 1) % self.fields.len();
                OverlayResult::Stay
            }
            Enter => match self.build() {
                Ok(spec) => OverlayResult::Commit(Action::CreateUser(spec)),
                Err(e)   => { self.error = Some(e); OverlayResult::Stay }
            },
            // Up/Down/Tab move between fields (above); everything else edits the
            // focused field, grapheme-aware with full cursor movement.
            _ => {
                let f = &mut self.fields[self.cursor];
                line_edit(&mut f.value, &mut f.cursor, key);
                OverlayResult::Stay
            }
        }
    }

    pub fn render(&self, buf: &mut Buffer, area: Rect) {
        let w = area.width.saturating_sub(8).clamp(34, 60);
        let h = (self.fields.len() as u16 + 4).min(area.height.saturating_sub(2));
        let rect = modal_frame(buf, area, w, h);
        btxt(buf, rect.x + 2, rect.y, "  new user  ", s_title());
        btxt(buf, rect.x + 2, rect.y + rect.height - 1,
             " Tab:field  Enter:create  Esc:cancel ", s_dim());

        let fx = rect.x + 2;
        let fw = rect.width.saturating_sub(4);
        for (i, f) in self.fields.iter().enumerate() {
            let y = rect.y + 1 + i as u16;
            let active = i == self.cursor;
            let lab = format!("{:>10}: ", f.label);
            btxt(buf, fx, y, &lab, if active { s_subhead() } else { s_dim() });
            let vx = fx + lab.len() as u16;
            let vw = fw.saturating_sub(lab.len() as u16);
            let opts = FieldRender {
                style: s_normal(),
                cursor_style: if active { s_sel() } else { s_normal() },
                mask: f.masked.then_some('•'),
            };
            let mut scroll = 0;
            render_field(buf, Rect::new(vx, y, vw, 1), &f.value, f.cursor, &mut scroll, &opts);
        }

        if let Some(err) = &self.error {
            let y = rect.y + 1 + self.fields.len() as u16;
            if y < rect.y + rect.height - 1 {
                btxt(buf, fx, y, &format!("⚠ {err}"), s_err());
            }
        }
    }
}
