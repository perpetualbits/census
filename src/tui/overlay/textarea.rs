//! Multi-line "big edit" modal for long attribute values.
//!
//! A generic editor for any editable attribute whose value benefits from more than
//! one line — `description`, certificates, `postalAddress`, long blobs. `Enter`
//! inserts a newline; `Ctrl+S` saves. Backed by mullion's stateless
//! [`textarea_edit`](mullion::textarea_edit) / [`render_textarea`](mullion::render_textarea);
//! commits the same [`Action::SetAttr`] the single-line editor does, so the write
//! and undo paths are unchanged.

use std::cell::Cell;

use mullion::{
    render_textarea, textarea_edit, Buffer, FieldRender, KeyCode, KeyModifiers, Rect, TextCtx,
};

use crate::tui::draw::btxt;
use crate::tui::theme::*;

use super::{modal_frame, Action, OverlayResult};

pub struct TextAreaDialog {
    title: String,
    dn: String,
    attr: String,
    value: String,
    cursor: usize,
    ctx: TextCtx,
    // Field geometry/scroll are set during `render` (which takes `&self`), so both
    // are `Cell`s the immutable render can update for the next `handle_key`.
    width: Cell<u16>,
    scroll_top: Cell<usize>,
}

impl TextAreaDialog {
    /// Big-edit attribute `attr` on `dn`, pre-filled with `current` (which may span
    /// multiple lines).
    pub fn edit_attr(dn: impl Into<String>, attr: impl Into<String>, current: &str) -> Self {
        let attr = attr.into();
        let value = current.to_string();
        Self {
            title: format!("edit {attr} (multi-line)"),
            dn: dn.into(),
            attr,
            cursor: value.len(),
            value,
            ctx: dctx(),
            width: Cell::new(72),
            scroll_top: Cell::new(0),
        }
    }

    pub fn handle_key(&mut self, key: KeyCode, mods: KeyModifiers) -> OverlayResult {
        use KeyCode::*;
        match key {
            Esc => OverlayResult::Cancel,
            // Ctrl+S saves the (possibly multi-line) value verbatim; empty clears it.
            Char('s') if mods.contains(KeyModifiers::CONTROL) => {
                let value = self.value.clone();
                OverlayResult::Commit(Action::SetAttr {
                    dn: self.dn.clone(),
                    attr: self.attr.clone(),
                    values: if value.is_empty() { vec![] } else { vec![value] },
                })
            }
            // Enter inserts a newline; arrows/Home/End/typing all go to the editor.
            _ => {
                textarea_edit(&mut self.value, &mut self.cursor, key, self.width.get(), self.ctx);
                OverlayResult::Stay
            }
        }
    }

    pub fn render(&self, buf: &mut Buffer, area: Rect) {
        let w = area.width.saturating_sub(6).clamp(30, 90);
        let h = area.height.saturating_sub(4).clamp(8, 30);
        let rect = modal_frame(buf, area, w, h);
        btxt(buf, rect.x + 2, rect.y, &format!("  {}  ", self.title), s_title());
        btxt(buf, rect.x + 2, rect.y + rect.height - 1,
             " Enter:newline  Ctrl+S:save  Esc:cancel ", s_dim());

        let field = Rect::new(rect.x + 2, rect.y + 1, rect.width.saturating_sub(4),
                              rect.height.saturating_sub(2));
        self.width.set(field.width);
        let opts = FieldRender { style: s_normal(), cursor_style: s_sel(), mask: None, ctx: self.ctx };
        let mut scroll = self.scroll_top.get();
        render_textarea(buf, field, &self.value, self.cursor, &mut scroll, &opts);
        self.scroll_top.set(scroll);
    }
}
