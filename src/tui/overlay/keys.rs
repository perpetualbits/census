//! SSH public-key manager modal.
//!
//! Lists the user's keys (one per row). The cursor selects a key to delete;
//! `a` enters an inline paste line to append a key. `s`/Enter commits the whole
//! set via [`Action::SetKeys`]; the editor is otherwise self-contained (it never
//! opens a nested overlay).

use mullion::{border::Borders, Buffer, KeyCode, KeyModifiers, Rect};

use crate::tui::draw::btxt;
use crate::tui::theme::*;

use super::{center, Action, OverlayResult};

pub struct KeyEditor {
    dn: String,
    keys: Vec<String>,
    cursor: usize,
    /// `Some` while pasting a new key line; holds the in-progress buffer.
    adding: Option<Vec<char>>,
}

impl KeyEditor {
    pub fn new(dn: impl Into<String>, keys: Vec<String>) -> Self {
        Self { dn: dn.into(), keys, cursor: 0, adding: None }
    }

    pub fn handle_key(&mut self, key: KeyCode, _mods: KeyModifiers) -> OverlayResult {
        use KeyCode::*;

        // Paste mode: keystrokes build the new key line.
        if let Some(buf) = &mut self.adding {
            match key {
                Esc => { self.adding = None; }
                Enter => {
                    let line: String = buf.iter().collect::<String>().trim().to_string();
                    if !line.is_empty() {
                        self.keys.push(line);
                        self.cursor = self.keys.len() - 1;
                    }
                    self.adding = None;
                }
                Char(c)   => buf.push(c),
                Backspace => { buf.pop(); }
                _ => {}
            }
            return OverlayResult::Stay;
        }

        // List mode.
        match key {
            Esc => OverlayResult::Cancel,
            Up   | Char('k') => { self.cursor = self.cursor.saturating_sub(1); OverlayResult::Stay }
            Down | Char('j') => {
                if self.cursor + 1 < self.keys.len() { self.cursor += 1; }
                OverlayResult::Stay
            }
            Char('d') => {
                if self.cursor < self.keys.len() {
                    self.keys.remove(self.cursor);
                    if self.cursor > 0 && self.cursor >= self.keys.len() {
                        self.cursor = self.keys.len().saturating_sub(1);
                    }
                }
                OverlayResult::Stay
            }
            Char('a') => { self.adding = Some(Vec::new()); OverlayResult::Stay }
            Char('s') | Enter => OverlayResult::Commit(Action::SetKeys {
                dn: self.dn.clone(),
                keys: self.keys.clone(),
            }),
            _ => OverlayResult::Stay,
        }
    }

    pub fn render(&self, buf: &mut Buffer, area: Rect) {
        let w = area.width.saturating_sub(6).clamp(40, 100);
        let h = (self.keys.len() as u16 + 6).clamp(8, area.height.saturating_sub(2));
        let rect = center(area, w, h);

        for y in rect.y..rect.y + rect.height {
            for x in rect.x..rect.x + rect.width {
                buf.set_string(x, y, " ", s_normal());
            }
        }
        mullion::border::draw_box(buf, rect, Borders::ALL, &box_style());
        btxt(buf, rect.x + 2, rect.y, "  ssh keys  ", s_title());

        let hint = if self.adding.is_some() {
            " Enter:add line  Esc:cancel "
        } else {
            " a:add  d:delete  s:save  Esc:cancel "
        };
        btxt(buf, rect.x + 2, rect.y + rect.height - 1, hint, s_dim());

        let inner_x = rect.x + 2;
        let inner_w = rect.width.saturating_sub(4);
        let list_y  = rect.y + 1;
        let list_h  = rect.height.saturating_sub(if self.adding.is_some() { 4 } else { 2 });

        if self.keys.is_empty() {
            btxt(buf, inner_x, list_y, "(no keys)", s_dim());
        }
        for (i, key) in self.keys.iter().enumerate().take(list_h as usize) {
            let y   = list_y + i as u16;
            let sel = i == self.cursor && self.adding.is_none();
            let sty = if sel { s_sel() } else { s_normal() };
            if sel {
                for x in inner_x..inner_x + inner_w { buf.set_string(x, y, " ", sty); }
            }
            let shown = truncate(&summarize(key), inner_w as usize);
            btxt(buf, inner_x, y, &shown, sty);
        }

        // Inline paste field.
        if let Some(b) = &self.adding {
            let fy = rect.y + rect.height - 2;
            for x in inner_x..inner_x + inner_w { buf.set_string(x, fy, " ", s_normal()); }
            let text: String = b.iter().collect();
            let shown = tail(&text, inner_w.saturating_sub(2) as usize);
            btxt(buf, inner_x, fy, &format!("> {shown}"), s_normal());
        }
    }
}

/// Condense a key to `type …tail comment` for the list.
fn summarize(key: &str) -> String {
    let mut p = key.split_whitespace();
    let kind = p.next().unwrap_or("key");
    let blob = p.next().unwrap_or("");
    let comment = p.next().unwrap_or("");
    let tail: String = blob.chars().rev().take(10).collect::<String>().chars().rev().collect();
    if comment.is_empty() { format!("{kind} …{tail}") } else { format!("{kind} …{tail}  {comment}") }
}

fn truncate(s: &str, w: usize) -> String {
    if s.chars().count() <= w { s.to_string() }
    else { s.chars().take(w.saturating_sub(1)).collect::<String>() + "…" }
}

/// Keep the last `w` chars (so the caret end of a long pasted line stays visible).
fn tail(s: &str, w: usize) -> String {
    let n = s.chars().count();
    if n <= w { s.to_string() } else { s.chars().skip(n - w).collect() }
}
