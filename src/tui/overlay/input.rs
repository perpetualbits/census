//! Single-line text input modal (attribute editing, and later form fields).

use mullion::{border::Borders, Buffer, KeyCode, KeyModifiers, Rect};

use crate::tui::draw::btxt;
use crate::tui::theme::*;

use super::{center, Action, OverlayResult};

/// What committing the input should produce.
enum Target {
    /// Replace a single attribute's value on `dn`.
    Attr { dn: String, attr: String },
}

/// A one-line text editor rendered as a centred modal box.
pub struct InputDialog {
    title: String,
    label: String,
    value: Vec<char>,
    cursor: usize, // char index in `value`, 0..=len
    masked: bool,
    target: Target,
}

impl InputDialog {
    /// Edit attribute `attr` on `dn`, pre-filled with `current`.
    pub fn edit_attr(dn: impl Into<String>, attr: impl Into<String>, current: &str) -> Self {
        let attr = attr.into();
        let value: Vec<char> = current.chars().collect();
        Self {
            title: "edit attribute".into(),
            label: attr.clone(),
            cursor: value.len(),
            value,
            masked: false,
            target: Target::Attr { dn: dn.into(), attr },
        }
    }

    fn text(&self) -> String { self.value.iter().collect() }

    pub fn handle_key(&mut self, key: KeyCode, _mods: KeyModifiers) -> OverlayResult {
        use KeyCode::*;
        match key {
            Esc => OverlayResult::Cancel,
            Enter => {
                let value = self.text();
                match &self.target {
                    Target::Attr { dn, attr } => OverlayResult::Commit(Action::SetAttr {
                        dn: dn.clone(),
                        attr: attr.clone(),
                        // An empty edit clears the attribute.
                        values: if value.is_empty() { vec![] } else { vec![value] },
                    }),
                }
            }
            Char(c) => { self.value.insert(self.cursor, c); self.cursor += 1; OverlayResult::Stay }
            Backspace => {
                if self.cursor > 0 { self.cursor -= 1; self.value.remove(self.cursor); }
                OverlayResult::Stay
            }
            Delete => {
                if self.cursor < self.value.len() { self.value.remove(self.cursor); }
                OverlayResult::Stay
            }
            Left  => { self.cursor = self.cursor.saturating_sub(1); OverlayResult::Stay }
            Right => { if self.cursor < self.value.len() { self.cursor += 1; } OverlayResult::Stay }
            Home  => { self.cursor = 0; OverlayResult::Stay }
            End   => { self.cursor = self.value.len(); OverlayResult::Stay }
            _ => OverlayResult::Stay,
        }
    }

    pub fn render(&self, buf: &mut Buffer, area: Rect) {
        let w = area.width.saturating_sub(8).clamp(20, 72);
        let rect = center(area, w, 6);
        // Clear the modal's interior so the screen below doesn't bleed through.
        for y in rect.y..rect.y + rect.height {
            for x in rect.x..rect.x + rect.width {
                buf.set_string(x, y, " ", s_normal());
            }
        }
        mullion::border::draw_box(buf, rect, Borders::ALL, &box_style());
        btxt(buf, rect.x + 2, rect.y, &format!("  {}  ", self.title), s_title());
        btxt(buf, rect.x + 2, rect.y + rect.height - 1, " Enter:save  Esc:cancel ", s_dim());

        // Label line.
        btxt(buf, rect.x + 2, rect.y + 1, &self.label, s_subhead());

        // Input field on the next line, inside a subtle frame.
        let field_y = rect.y + 3;
        let fx = rect.x + 2;
        let fw = rect.width.saturating_sub(4);
        for x in fx..fx + fw {
            buf.set_string(x, field_y, " ", s_normal());
        }
        let shown: String = if self.masked {
            "•".repeat(self.value.len())
        } else {
            self.text()
        };
        // Scroll the text so the cursor stays visible within the field.
        let fw = fw as usize;
        let start = self.cursor.saturating_sub(fw.saturating_sub(1));
        let visible: String = shown.chars().skip(start).take(fw).collect();
        btxt(buf, fx, field_y, &visible, s_normal());

        // Cursor block.
        let cx = fx + (self.cursor - start) as u16;
        if cx < fx + fw as u16 {
            let under: String = shown.chars().nth(self.cursor).map(|c| c.to_string())
                .unwrap_or_else(|| " ".into());
            buf.set_string(cx, field_y, &under, s_sel());
        }
    }
}
