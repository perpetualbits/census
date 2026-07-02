//! New-group form modal: collects a group name and gidNumber.

use mullion::{
    line_edit, render_field, render_validity, Buffer, FieldRender, FormLayout, FormRow, KeyCode,
    KeyModifiers, Rect, TextCtx, Validity,
};

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

        // Two label:field rows (spaced) laid out with the mullion form primitive.
        let layout = FormLayout { label_cols: 11, gap: 1, status_cols: 0, row_height: 2 };
        let rows = layout.rows(Rect::new(rect.x + 2, rect.y + 1, rect.width.saturating_sub(4), 4), 2, TextCtx::LTR);

        field_row(buf, "name", &self.name, self.name_cur, self.field == 0, &rows[0]);
        field_row(buf, "gidNumber", &self.gid, self.gid_cur, self.field == 1, &rows[1]);

        if let Some(err) = &self.error {
            let status = Rect::new(rect.x + 2, rect.y + 5, rect.width.saturating_sub(4), 1);
            render_validity(buf, status, &Validity::Error(err.clone()), &mullion_theme());
        }
    }
}

/// Render one labelled field into a resolved [`FormRow`]; the active one shows the cursor.
fn field_row(buf: &mut Buffer, label: &str, val: &str, cursor: usize, active: bool, row: &FormRow) {
    let lab = format!("{label}:");
    let lx = row.label.x + row.label.width.saturating_sub(lab.chars().count() as u16);
    btxt(buf, lx, row.label.y, &lab, if active { s_subhead() } else { s_dim() });
    let opts = FieldRender {
        style: s_normal(),
        cursor_style: if active { s_sel() } else { s_normal() },
        mask: None,
        ctx: TextCtx::LTR,
    };
    let mut scroll = 0;
    render_field(buf, row.field, val, cursor, &mut scroll, &opts);
}
