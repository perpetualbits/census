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

/// What a confirmation commits: a normal entry [`Action`], or a domain deletion
/// (handled off the `Action`/undo path, since it drops a whole naming context + session).
enum Outcome {
    Act(Action),
    DeleteDomain { session_idx: usize },
    /// Run the pending host-provisioning plan (OpenLDAP domain create/delete).
    RunProvision,
}

pub struct ConfirmDialog {
    prompt: String,
    kind: ConfirmKind,
    typed: String,
    cursor: usize,
    outcome: Outcome,
}

impl ConfirmDialog {
    pub fn yes_no(prompt: impl Into<String>, action: Action) -> Self {
        Self { prompt: prompt.into(), kind: ConfirmKind::YesNo, typed: String::new(), cursor: 0, outcome: Outcome::Act(action) }
    }

    pub fn typed_dn(prompt: impl Into<String>, expected: impl Into<String>, action: Action) -> Self {
        Self {
            prompt: prompt.into(),
            kind: ConfirmKind::TypedDn { expected: expected.into() },
            typed: String::new(),
            cursor: 0,
            outcome: Outcome::Act(action),
        }
    }

    /// Confirm deleting the domain that session `session_idx` is on — the operator must
    /// type the suffix exactly.
    pub fn delete_domain(prompt: impl Into<String>, suffix: impl Into<String>, session_idx: usize) -> Self {
        Self {
            prompt: prompt.into(),
            kind: ConfirmKind::TypedDn { expected: suffix.into() },
            typed: String::new(),
            cursor: 0,
            outcome: Outcome::DeleteDomain { session_idx },
        }
    }

    /// A `y/N` review of a pending host-provisioning plan; `prompt` may be multi-line
    /// (the exact commands census will run).
    pub fn review_provision(prompt: impl Into<String>) -> Self {
        Self {
            prompt: prompt.into(),
            kind: ConfirmKind::YesNo,
            typed: String::new(),
            cursor: 0,
            outcome: Outcome::RunProvision,
        }
    }

    /// The result to emit once confirmed.
    fn confirmed(&self) -> OverlayResult {
        match &self.outcome {
            Outcome::Act(a) => OverlayResult::Commit(a.clone()),
            Outcome::DeleteDomain { session_idx } => OverlayResult::DeleteDomain { session_idx: *session_idx },
            Outcome::RunProvision => OverlayResult::RunProvision,
        }
    }

    pub fn handle_key(&mut self, key: KeyCode, _mods: KeyModifiers) -> OverlayResult {
        use KeyCode::*;
        match &self.kind {
            ConfirmKind::YesNo => match key {
                Char('y') | Char('Y') => self.confirmed(),
                _ => OverlayResult::Cancel,
            },
            ConfirmKind::TypedDn { expected } => match key {
                Esc => OverlayResult::Cancel,
                Enter => {
                    if &self.typed == expected {
                        self.confirmed()
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
        // YesNo prompts may be multi-line (a review of exact commands); size to fit.
        let lines: Vec<&str> = self.prompt.split('\n').collect();
        let w = area.width.saturating_sub(6).clamp(24, 96);
        let h = match self.kind {
            ConfirmKind::YesNo => 3 + lines.len() as u16,
            ConfirmKind::TypedDn { .. } => 7,
        };
        let rect = modal_frame(buf, area, w, h);
        btxt(buf, rect.x + 2, rect.y, "  confirm  ", s_title());

        match &self.kind {
            ConfirmKind::YesNo => {
                for (i, line) in lines.iter().enumerate() {
                    btxt(buf, rect.x + 2, rect.y + 1 + i as u16, line, s_normal());
                }
                btxt(buf, rect.x + 2, rect.y + rect.height - 1, " y:yes  any other:cancel ", s_dim());
            }
            ConfirmKind::TypedDn { expected } => {
                btxt(buf, rect.x + 2, rect.y + 1, &self.prompt, s_normal());
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
