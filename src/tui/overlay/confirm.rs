//! Confirmation modal: simple `y/N` for destructive ops, or typed-DN for the
//! irreversible ones (deleting an entry — the operator must type the full DN).

use mullion::{border::Borders, Buffer, KeyCode, KeyModifiers, Rect};

use crate::tui::draw::btxt;
use crate::tui::theme::*;

use super::{center, Action, OverlayResult};

pub enum ConfirmKind {
    /// Press `y` to confirm; anything else cancels.
    YesNo,
    /// Type `expected` exactly, then Enter, to confirm.
    TypedDn { expected: String },
}

pub struct ConfirmDialog {
    prompt: String,
    kind: ConfirmKind,
    typed: Vec<char>,
    action: Action,
}

impl ConfirmDialog {
    pub fn yes_no(prompt: impl Into<String>, action: Action) -> Self {
        Self { prompt: prompt.into(), kind: ConfirmKind::YesNo, typed: Vec::new(), action }
    }

    #[allow(dead_code)] // wired up by entry deletion (P8)
    pub fn typed_dn(prompt: impl Into<String>, expected: impl Into<String>, action: Action) -> Self {
        Self {
            prompt: prompt.into(),
            kind: ConfirmKind::TypedDn { expected: expected.into() },
            typed: Vec::new(),
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
                    let typed: String = self.typed.iter().collect();
                    if &typed == expected {
                        OverlayResult::Commit(self.action.clone())
                    } else {
                        OverlayResult::Stay
                    }
                }
                Char(c)   => { self.typed.push(c); OverlayResult::Stay }
                Backspace => { self.typed.pop(); OverlayResult::Stay }
                _ => OverlayResult::Stay,
            },
        }
    }

    pub fn render(&self, buf: &mut Buffer, area: Rect) {
        let w = area.width.saturating_sub(8).clamp(24, 80);
        let h = match self.kind { ConfirmKind::YesNo => 5, ConfirmKind::TypedDn { .. } => 7 };
        let rect = center(area, w, h);

        for y in rect.y..rect.y + rect.height {
            for x in rect.x..rect.x + rect.width {
                buf.set_string(x, y, " ", s_normal());
            }
        }
        mullion::border::draw_box(buf, rect, Borders::ALL, &box_style());
        btxt(buf, rect.x + 2, rect.y, "  confirm  ", s_title());
        btxt(buf, rect.x + 2, rect.y + 1, &self.prompt, s_normal());

        match &self.kind {
            ConfirmKind::YesNo => {
                btxt(buf, rect.x + 2, rect.y + rect.height - 1, " y:yes  any other:cancel ", s_dim());
            }
            ConfirmKind::TypedDn { expected } => {
                btxt(buf, rect.x + 2, rect.y + 2, &format!("type: {expected}"), s_dim());
                let fy = rect.y + 4;
                let fw = rect.width.saturating_sub(4);
                for x in rect.x + 2..rect.x + 2 + fw { buf.set_string(x, fy, " ", s_normal()); }
                let typed: String = self.typed.iter().collect();
                let matches = &typed == expected;
                btxt(buf, rect.x + 2, fy, &typed, if matches { s_ok() } else { s_err() });
                btxt(buf, rect.x + 2, rect.y + rect.height - 1,
                     " Enter:confirm (must match)  Esc:cancel ", s_dim());
            }
        }
    }
}
