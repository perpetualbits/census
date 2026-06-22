//! Pane identity and the cursor/offset bookkeeping every list view shares.

/// Which side of a two-pane screen currently has focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pane { Left, Right }

/// A scrollable list's selected row plus the scroll offset that keeps it in view.
#[derive(Debug, Clone, Copy, Default)]
pub struct ListCursor {
    pub cursor: usize,
    pub offset: usize,
}

impl ListCursor {
    pub fn new() -> Self { Self::default() }

    /// Reset to the top of the list.
    pub fn reset(&mut self) { self.cursor = 0; self.offset = 0; }

    /// Move the cursor up one row (saturating at 0).
    pub fn up(&mut self) { self.cursor = self.cursor.saturating_sub(1); }

    /// Move the cursor down one row, clamped to `len`.
    pub fn down(&mut self, len: usize) {
        if self.cursor + 1 < len { self.cursor += 1; }
    }

    /// Jump by `delta` rows (e.g. PageUp/PageDown), clamped to `[0, len)`.
    pub fn page(&mut self, delta: isize, len: usize) {
        let max = len.saturating_sub(1) as isize;
        let next = (self.cursor as isize + delta).clamp(0, max.max(0));
        self.cursor = next as usize;
    }

    /// Clamp the cursor so it cannot point past the end of a (possibly shrunk) list.
    pub fn clamp(&mut self, len: usize) {
        if len > 0 && self.cursor >= len { self.cursor = len - 1; }
    }

    /// Adjust `offset` so `cursor` stays within a window of `visible` rows.
    pub fn keep_in_view(&mut self, visible: usize) {
        if visible == 0 { self.offset = 0; return; }
        if self.cursor < self.offset {
            self.offset = self.cursor;
        } else if self.cursor >= self.offset + visible {
            self.offset = self.cursor + 1 - visible;
        }
    }
}
