//! Confirmation modal: simple `y/N` for destructive ops, or typed-DN for the
//! irreversible ones (deleting an entry — the operator must type the full DN).

use mullion::{line_edit, render_field, Buffer, FieldRender, KeyCode, KeyModifiers, Rect};

use crate::tui::draw::btxt;
use crate::tui::theme::*;

use super::{modal_frame, Action, OverlayResult};

pub enum ConfirmKind {
    /// Press `y` to confirm; anything else cancels.
    YesNo,
    /// Type `expected` exactly, then Enter, to confirm.
    TypedDn { expected: String },
}

pub struct ConfirmDialog {
    prompt: String,
    kind: ConfirmKind,
    typed: String,
    cursor: usize,
    action: Action,
}

impl ConfirmDialog {
    pub fn yes_no(prompt: impl Into<String>, action: Action) -> Self {
        Self { prompt: prompt.into(), kind: ConfirmKind::YesNo, typed: String::new(), cursor: 0, action }
    }

    pub fn typed_dn(prompt: impl Into<String>, expected: impl Into<String>, action: Action) -> Self {
        Self {
            prompt: prompt.into(),
            kind: ConfirmKind::TypedDn { expected: expected.into() },
            typed: String::new(),
            cursor: 0,
            action,
        }
    }

    pub fn handle_key(&mut self, key: KeyCode, _mods: KeyModifiers) -> OverlayResult {
        use KeyCode::*;
        match &self.kind {
            ConfirmKind::YesNo => match key {
                Char('y') | Char('Y') => OverlayResult::Commit(self.action.clone()),
                _ => OverlayResult::Cancel,
            },
            ConfirmKind::TypedDn { expected } => match key {
                Esc => OverlayResult::Cancel,
                Enter => {
                    if &self.typed == expected {
                        OverlayResult::Commit(self.action.clone())
                    } else {
                        OverlayResult::Stay
                    }
                }
                _ => { line_edit(&mut self.typed, &mut self.cursor, key); OverlayResult::Stay }
            },
        }
    }

    /// Only the typed-DN variant has a text field; pasting the DN is a convenience.
    pub fn handle_paste(&mut self, text: &str) -> OverlayResult {
        if matches!(self.kind, ConfirmKind::TypedDn { .. }) {
            super::paste_into(&mut self.typed, &mut self.cursor, text, false);
        }
        OverlayResult::Stay
    }

    pub fn render(&self, buf: &mut Buffer, area: Rect) {
        let w = area.width.saturating_sub(8).clamp(24, 80);
        let h = match self.kind { ConfirmKind::YesNo => 5, ConfirmKind::TypedDn { .. } => 7 };
        let rect = modal_frame(buf, area, w, h);
        btxt(buf, rect.x + 2, rect.y, "  confirm  ", s_title());
        btxt(buf, rect.x + 2, rect.y + 1, &self.prompt, s_normal());

        match &self.kind {
            ConfirmKind::YesNo => {
                btxt(buf, rect.x + 2, rect.y + rect.height - 1, " y:yes  any other:cancel ", s_dim());
            }
            ConfirmKind::TypedDn { expected } => {
                btxt(buf, rect.x + 2, rect.y + 2, &format!("type: {expected}"), s_dim());
                let matches = &self.typed == expected;
                // The field text is coloured green once it matches, red until then.
                let opts = FieldRender {
                    style: if matches { s_ok() } else { s_err() },
                    cursor_style: s_sel(),
                    mask: None,
                    ctx: mullion::TextCtx::LTR,
                };
                let fw = rect.width.saturating_sub(4);
                let mut scroll = 0;
                render_field(buf, Rect::new(rect.x + 2, rect.y + 4, fw, 1), &self.typed, self.cursor, &mut scroll, &opts);
                btxt(buf, rect.x + 2, rect.y + rect.height - 1,
                     " Enter:confirm (must match)  Esc:cancel ", s_dim());
            }
        }
    }
}
