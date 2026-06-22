//! New-group form modal: collects a group name and gidNumber.

use mullion::{border::Borders, Buffer, KeyCode, KeyModifiers, Rect};

use crate::tui::draw::btxt;
use crate::tui::theme::*;

use super::{center, Action, OverlayResult};

pub struct NewGroupForm {
    name: Vec<char>,
    gid: Vec<char>,
    /// 0 = name field, 1 = gidNumber field.
    field: u8,
    error: Option<String>,
}

impl NewGroupForm {
    /// Seed the form; `gid_number` pre-fills the gidNumber field.
    pub fn new(gid_number: u32) -> Self {
        Self {
            name: Vec::new(),
            gid: gid_number.to_string().chars().collect(),
            field: 0,
            error: None,
        }
    }

    fn cur(&mut self) -> &mut Vec<char> {
        if self.field == 0 { &mut self.name } else { &mut self.gid }
    }

    fn build(&self) -> Result<Action, String> {
        let name: String = self.name.iter().collect();
        if name.is_empty() { return Err("group name is required".into()); }
        let gid_number = self.gid.iter().collect::<String>().parse::<u32>()
            .map_err(|_| "gidNumber must be a number".to_string())?;
        Ok(Action::CreateGroup { name, gid_number })
    }

    pub fn handle_key(&mut self, key: KeyCode, _mods: KeyModifiers) -> OverlayResult {
        use KeyCode::*;
        match key {
            Esc => OverlayResult::Cancel,
            Tab | Down | Up | BackTab => { self.field ^= 1; OverlayResult::Stay }
            Enter => match self.build() {
                Ok(action) => OverlayResult::Commit(action),
                Err(e)     => { self.error = Some(e); OverlayResult::Stay }
            },
            Char(c)   => { self.cur().push(c); OverlayResult::Stay }
            Backspace => { self.cur().pop(); OverlayResult::Stay }
            _ => OverlayResult::Stay,
        }
    }

    pub fn render(&self, buf: &mut Buffer, area: Rect) {
        let w = area.width.saturating_sub(8).clamp(30, 56);
        let rect = center(area, w, 7);

        for y in rect.y..rect.y + rect.height {
            for x in rect.x..rect.x + rect.width {
                buf.set_string(x, y, " ", s_normal());
            }
        }
        mullion::border::draw_box(buf, rect, Borders::ALL, &box_style());
        btxt(buf, rect.x + 2, rect.y, "  new group  ", s_title());
        btxt(buf, rect.x + 2, rect.y + rect.height - 1,
             " Tab:field  Enter:create  Esc:cancel ", s_dim());

        let fx = rect.x + 2;
        let fw = rect.width.saturating_sub(4);
        self.field_line(buf, fx, rect.y + 1, fw, "name", &self.name, self.field == 0);
        self.field_line(buf, fx, rect.y + 3, fw, "gidNumber", &self.gid, self.field == 1);

        if let Some(err) = &self.error {
            btxt(buf, fx, rect.y + 5, &format!("⚠ {err}"), s_err());
        }
    }

    #[allow(clippy::too_many_arguments)] // a private render helper; args are all positional draw params
    fn field_line(&self, buf: &mut Buffer, x: u16, y: u16, w: u16, label: &str, val: &[char], active: bool) {
        let lab = format!("{label:>10}: ");
        btxt(buf, x, y, &lab, if active { s_subhead() } else { s_dim() });
        let vx = x + lab.len() as u16;
        let vw = w.saturating_sub(lab.len() as u16);
        if active {
            for cx in vx..vx + vw { buf.set_string(cx, y, " ", s_normal()); }
        }
        let text: String = val.iter().collect();
        btxt(buf, vx, y, &text, s_normal());
        if active {
            let cx = vx + text.chars().count() as u16;
            if cx < vx + vw { buf.set_string(cx, y, " ", s_sel()); }
        }
    }
}
