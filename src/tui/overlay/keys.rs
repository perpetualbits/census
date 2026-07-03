//! SSH public-key manager modal.
//!
//! Lists the user's keys (one per row). The cursor selects a key to delete;
//! `a` enters an inline paste line to append a key. `s`/Enter commits the whole
//! set via [`Action::SetKeys`]; the editor is otherwise self-contained (it never
//! opens a nested overlay).

use mullion::{
    diff_lines, line_edit, render_diff_unified, render_field, Buffer, FieldRender, KeyCode,
    KeyModifiers, Rect, TextCtx,
};

use crate::tui::draw::btxt;
use crate::tui::theme::*;

use super::{modal_frame, Action, OverlayResult};

pub struct KeyEditor {
    dn: String,
    keys: Vec<String>,
    /// The key set at open time, for the "changes" diff.
    original: Vec<String>,
    cursor: usize,
    /// `Some` while pasting a new key line; holds the `(buffer, cursor)` being typed.
    adding: Option<(String, usize)>,
}

impl KeyEditor {
    pub fn new(dn: impl Into<String>, keys: Vec<String>) -> Self {
        Self { dn: dn.into(), original: keys.clone(), keys, cursor: 0, adding: None }
    }

    pub fn handle_key(&mut self, key: KeyCode, _mods: KeyModifiers) -> OverlayResult {
        use KeyCode::*;

        // Paste mode: keystrokes build the new key line.
        if let Some((text, cur)) = &mut self.adding {
            match key {
                Esc => { self.adding = None; }
                Enter => {
                    let line = text.trim().to_string();
                    if !line.is_empty() {
                        self.keys.push(line);
                        self.cursor = self.keys.len() - 1;
                    }
                    self.adding = None;
                }
                _ => { line_edit(text, cur, key); }
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
            Char('a') => { self.adding = Some((String::new(), 0)); OverlayResult::Stay }
            Char('s') | Enter => OverlayResult::Commit(Action::SetKeys {
                dn: self.dn.clone(),
                keys: self.keys.clone(),
            }),
            _ => OverlayResult::Stay,
        }
    }

    /// A bracketed paste is how a (long) key actually gets in. If an add-line is open,
    /// the paste fills it (whitespace collapsed to one line — an OpenSSH key is one
    /// line). Otherwise each pasted non-empty line is appended as its own key, so a
    /// single key or a whole `authorized_keys` block both work — and, crucially, the
    /// pasted text never runs as `a`/`d`/`s` commands.
    pub fn handle_paste(&mut self, text: &str) -> OverlayResult {
        if let Some((buf, cur)) = &mut self.adding {
            let clean = text.split_whitespace().collect::<Vec<_>>().join(" ");
            let at = (*cur).min(buf.len());
            buf.insert_str(at, &clean);
            *cur = at + clean.len();
        } else {
            for line in text.lines() {
                let line = line.trim();
                if !line.is_empty() {
                    self.keys.push(line.to_string());
                    self.cursor = self.keys.len() - 1;
                }
            }
        }
        OverlayResult::Stay
    }

    pub fn render(&self, buf: &mut Buffer, area: Rect) {
        // The pending change, as a diff of summarised key sets (added +, removed -).
        let orig: Vec<String> = self.original.iter().map(|k| summarize(k)).collect();
        let curr: Vec<String> = self.keys.iter().map(|k| summarize(k)).collect();
        let orig_refs: Vec<&str> = orig.iter().map(String::as_str).collect();
        let curr_refs: Vec<&str> = curr.iter().map(String::as_str).collect();
        let ops = diff_lines(&orig_refs, &curr_refs);
        let changed = self.keys != self.original;
        // Rows the diff panel needs (a header + the ops, capped), 0 when unchanged.
        let diff_rows = if changed { (ops.len().min(5) as u16) + 1 } else { 0 };

        let w = area.width.saturating_sub(6).clamp(40, 100);
        let h = (self.keys.len() as u16 + 6 + diff_rows).clamp(8, area.height.saturating_sub(2));
        let rect = modal_frame(buf, area, w, h);
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
        let list_h  = rect.height.saturating_sub((if self.adding.is_some() { 4 } else { 2 }) + diff_rows);

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

        // Inline paste field: "> " prompt, then the scrolling editable line.
        if let Some((text, cur)) = &self.adding {
            let fy = rect.y + rect.height - 2;
            btxt(buf, inner_x, fy, "> ", s_dim());
            let fx = inner_x + 2;
            let fw = inner_w.saturating_sub(2);
            let opts = FieldRender { style: s_normal(), cursor_style: s_sel(), mask: None, ctx: mullion::TextCtx::LTR };
            let mut scroll = 0;
            render_field(buf, Rect::new(fx, fy, fw, 1), text, *cur, &mut scroll, &opts);
        }

        // "Changes" panel: a unified diff of the key set vs how it was opened.
        if changed && diff_rows > 1 {
            let dy = list_y + list_h;
            btxt(buf, inner_x, dy, "changes:", s_dim());
            let drect = Rect::new(inner_x, dy + 1, inner_w, diff_rows - 1);
            let mut top = 0;
            render_diff_unified(buf, drect, &ops, &mut top, &mullion_theme(), TextCtx::LTR);
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
