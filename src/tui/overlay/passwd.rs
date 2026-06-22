//! Set-password modal: two masked fields that must match before committing.

use mullion::{border::Borders, Buffer, KeyCode, KeyModifiers, Rect};

use crate::tui::draw::btxt;
use crate::tui::theme::*;

use super::{center, Action, OverlayResult};

pub struct PasswdDialog {
    dn: String,
    uid: String,
    first: Vec<char>,
    second: Vec<char>,
    /// 0 = editing the first field, 1 = the confirmation field.
    field: u8,
}

impl PasswdDialog {
    pub fn new(dn: impl Into<String>, uid: impl Into<String>) -> Self {
        Self { dn: dn.into(), uid: uid.into(), first: Vec::new(), second: Vec::new(), field: 0 }
    }

    fn cur(&mut self) -> &mut Vec<char> {
        if self.field == 0 { &mut self.first } else { &mut self.second }
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
                        plaintext: self.first.iter().collect(),
                    })
                } else {
                    OverlayResult::Stay
                }
            }
            Char(c)   => { self.cur().push(c); OverlayResult::Stay }
            Backspace => { self.cur().pop(); OverlayResult::Stay }
            _ => OverlayResult::Stay,
        }
    }

    pub fn render(&self, buf: &mut Buffer, area: Rect) {
        let w = area.width.saturating_sub(8).clamp(28, 60);
        let rect = center(area, w, 8);

        for y in rect.y..rect.y + rect.height {
            for x in rect.x..rect.x + rect.width {
                buf.set_string(x, y, " ", s_normal());
            }
        }
        mullion::border::draw_box(buf, rect, Borders::ALL, &box_style());
        btxt(buf, rect.x + 2, rect.y, &format!("  set password: {}  ", self.uid), s_title());
        btxt(buf, rect.x + 2, rect.y + rect.height - 1,
             " Tab:field  Enter:save  Esc:cancel ", s_dim());

        let fw = rect.width.saturating_sub(4);
        self.field_line(buf, rect.x + 2, rect.y + 1, fw, "new", &self.first, self.field == 0);
        self.field_line(buf, rect.x + 2, rect.y + 3, fw, "confirm", &self.second, self.field == 1);

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

    #[allow(clippy::too_many_arguments)] // a private render helper; args are all positional draw params
    fn field_line(&self, buf: &mut Buffer, x: u16, y: u16, w: u16, label: &str, val: &[char], active: bool) {
        let lab = format!("{label:>8}: ");
        let lab_sty = if active { s_subhead() } else { s_dim() };
        btxt(buf, x, y, &lab, lab_sty);
        let fx = x + lab.len() as u16;
        let fw = w.saturating_sub(lab.len() as u16);
        for cx in fx..fx + fw { buf.set_string(cx, y, " ", s_normal()); }
        let dots = "•".repeat(val.len().min(fw as usize));
        btxt(buf, fx, y, &dots, s_normal());
        if active {
            let cx = fx + dots.chars().count() as u16;
            if cx < fx + fw { buf.set_string(cx, y, " ", s_sel()); }
        }
    }
}
