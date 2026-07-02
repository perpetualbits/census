//! New-group form modal: collects a group name and gidNumber.

use mullion::{line_edit, render_field, Buffer, FieldRender, KeyCode, KeyModifiers, Rect};

use crate::tui::draw::btxt;
use crate::tui::theme::*;

use super::{modal_frame, Action, OverlayResult};

pub struct NewGroupForm {
    name: String,
    name_cur: usize,
    gid: String,
    gid_cur: usize,
    /// 0 = name field, 1 = gidNumber field.
    field: u8,
    error: Option<String>,
}

impl NewGroupForm {
    /// Seed the form; `gid_number` pre-fills the gidNumber field.
    pub fn new(gid_number: u32) -> Self {
        let gid = gid_number.to_string();
        Self {
            name: String::new(), name_cur: 0,
            gid_cur: gid.len(), gid,
            field: 0,
            error: None,
        }
    }

    /// The active field's `(text, cursor)`.
    fn cur(&mut self) -> (&mut String, &mut usize) {
        if self.field == 0 { (&mut self.name, &mut self.name_cur) }
        else { (&mut self.gid, &mut self.gid_cur) }
    }

    fn build(&self) -> Result<Action, String> {
        if self.name.is_empty() { return Err("group name is required".into()); }
        let gid_number = self.gid.parse::<u32>()
            .map_err(|_| "gidNumber must be a number".to_string())?;
        Ok(Action::CreateGroup { name: self.name.clone(), gid_number })
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
            _ => { let (t, c) = self.cur(); line_edit(t, c, key); OverlayResult::Stay }
        }
    }

    pub fn render(&self, buf: &mut Buffer, area: Rect) {
        let w = area.width.saturating_sub(8).clamp(30, 56);
        let rect = modal_frame(buf, area, w, 7);
        btxt(buf, rect.x + 2, rect.y, "  new group  ", s_title());
        btxt(buf, rect.x + 2, rect.y + rect.height - 1,
             " Tab:field  Enter:create  Esc:cancel ", s_dim());

        let fx = rect.x + 2;
        let fw = rect.width.saturating_sub(4);
        field_line(buf, fx, rect.y + 1, fw, "name", &self.name, self.name_cur, self.field == 0);
        field_line(buf, fx, rect.y + 3, fw, "gidNumber", &self.gid, self.gid_cur, self.field == 1);

        if let Some(err) = &self.error {
            btxt(buf, fx, rect.y + 5, &format!("⚠ {err}"), s_err());
        }
    }
}

/// One labelled field line; the active one shows the cursor.
#[allow(clippy::too_many_arguments)] // a private render helper; args are all positional draw params
fn field_line(buf: &mut Buffer, x: u16, y: u16, w: u16, label: &str, val: &str, cursor: usize, active: bool) {
    let lab = format!("{label:>10}: ");
    btxt(buf, x, y, &lab, if active { s_subhead() } else { s_dim() });
    let vx = x + lab.len() as u16;
    let vw = w.saturating_sub(lab.len() as u16);
    let opts = FieldRender {
        style: s_normal(),
        cursor_style: if active { s_sel() } else { s_normal() },
        mask: None,
        ctx: mullion::TextCtx::LTR,
    };
    let mut scroll = 0;
    render_field(buf, Rect::new(vx, y, vw, 1), val, cursor, &mut scroll, &opts);
}
