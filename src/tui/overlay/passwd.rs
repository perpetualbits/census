//! Set-password modal: two masked fields that must match before committing.

use mullion::{line_edit, render_field, Buffer, FieldRender, KeyCode, KeyModifiers, Rect};

use crate::tui::draw::btxt;
use crate::tui::theme::*;

use super::{modal_frame, Action, OverlayResult};

pub struct PasswdDialog {
    dn: String,
    uid: String,
    first: String,
    first_cur: usize,
    second: String,
    second_cur: usize,
    /// 0 = editing the first field, 1 = the confirmation field.
    field: u8,
}

impl PasswdDialog {
    pub fn new(dn: impl Into<String>, uid: impl Into<String>) -> Self {
        Self {
            dn: dn.into(), uid: uid.into(),
            first: String::new(), first_cur: 0,
            second: String::new(), second_cur: 0,
            field: 0,
        }
    }

    /// The active field's `(text, cursor)`.
    fn cur(&mut self) -> (&mut String, &mut usize) {
        if self.field == 0 { (&mut self.first, &mut self.first_cur) }
        else { (&mut self.second, &mut self.second_cur) }
    }

    fn matches(&self) -> bool {
        !self.first.is_empty() && self.first == self.second
    }

    pub fn handle_key(&mut self, key: KeyCode, _mods: KeyModifiers) -> OverlayResult {
        use KeyCode::*;
        match key {
            Esc => OverlayResult::Cancel,
            Tab | Down | Up | BackTab => { self.field ^= 1; OverlayResult::Stay }
            Enter => {
                if self.field == 0 {
                    self.field = 1;
                    OverlayResult::Stay
                } else if self.matches() {
                    OverlayResult::Commit(Action::SetPasswd {
                        dn: self.dn.clone(),
                        plaintext: self.first.clone(),
                    })
                } else {
                    OverlayResult::Stay
                }
            }
            _ => { let (t, c) = self.cur(); line_edit(t, c, key); OverlayResult::Stay }
        }
    }

    pub fn render(&self, buf: &mut Buffer, area: Rect) {
        let w = area.width.saturating_sub(8).clamp(28, 60);
        let rect = modal_frame(buf, area, w, 8);
        btxt(buf, rect.x + 2, rect.y, &format!("  set password: {}  ", self.uid), s_title());
        btxt(buf, rect.x + 2, rect.y + rect.height - 1,
             " Tab:field  Enter:save  Esc:cancel ", s_dim());

        let fw = rect.width.saturating_sub(4);
        field_line(buf, rect.x + 2, rect.y + 1, fw, "new", &self.first, self.first_cur, self.field == 0);
        field_line(buf, rect.x + 2, rect.y + 3, fw, "confirm", &self.second, self.second_cur, self.field == 1);

        // Match indicator.
        let (msg, sty) = if self.first.is_empty() && self.second.is_empty() {
            ("", s_dim())
        } else if self.matches() {
            ("✓ match", s_ok())
        } else {
            ("✗ differ", s_err())
        };
        btxt(buf, rect.x + 2, rect.y + 5, msg, sty);
    }
}

/// One labelled, masked field line; the active one shows the cursor.
#[allow(clippy::too_many_arguments)] // a private render helper; args are all positional draw params
fn field_line(buf: &mut Buffer, x: u16, y: u16, w: u16, label: &str, val: &str, cursor: usize, active: bool) {
    let lab = format!("{label:>8}: ");
    btxt(buf, x, y, &lab, if active { s_subhead() } else { s_dim() });
    let fx = x + lab.len() as u16;
    let fw = w.saturating_sub(lab.len() as u16);
    let opts = FieldRender {
        style: s_normal(),
        cursor_style: if active { s_sel() } else { s_normal() },
        mask: Some('•'),
    };
    let mut scroll = 0;
    render_field(buf, Rect::new(fx, y, fw, 1), val, cursor, &mut scroll, &opts);
}
