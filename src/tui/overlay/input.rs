//! Single-line text input modal (attribute editing, and later form fields).

use mullion::{
    line_edit, render_field, visual_step, Buffer, Direction, FieldRender, KeyCode, KeyModifiers,
    Rect, TextCtx,
};

use crate::tui::draw::btxt;
use crate::tui::theme::*;

use super::{modal_frame, Action, OverlayResult};

/// What committing the input should produce.
enum Target {
    /// Replace a single attribute's value on `dn`.
    Attr { dn: String, attr: String },
    /// Rename a group: change its `cn` RDN to the typed value.
    Rename { dn: String, old_name: String },
    /// Create a new domain (naming context) on the server of session `template_idx`.
    NewDomain { template_idx: usize },
    /// Add a schema definition (`kind`) to the server of session `session_idx`.
    NewSchema { session_idx: usize, kind: crate::ldap::client::SchemaKind },
}

/// A one-line text editor rendered as a centred modal box. The text buffer and
/// cursor are owned here; mullion's [`line_edit`]/[`render_field`] primitives do the
/// grapheme-aware editing and horizontally-scrolling render.
pub struct InputDialog {
    title: String,
    label: String,
    value: String,
    cursor: usize, // byte index into `value`, kept on a grapheme boundary
    masked: bool,
    /// Base direction for the field: caret motion follows *visual* order and the
    /// glyphs shape for this context. `dctx()` (auto-detect) so RTL/mixed values edit right.
    ctx: TextCtx,
    target: Target,
}

impl InputDialog {
    /// Edit attribute `attr` on `dn`, pre-filled with `current`.
    pub fn edit_attr(dn: impl Into<String>, attr: impl Into<String>, current: &str) -> Self {
        let attr = attr.into();
        let value = current.to_string();
        Self {
            title: "edit attribute".into(),
            label: attr.clone(),
            cursor: value.len(),
            value,
            masked: false,
            ctx: dctx(),
            target: Target::Attr { dn: dn.into(), attr },
        }
    }

    /// Rename group `current_name` (DN `dn`) — the typed value becomes its new `cn` RDN.
    pub fn rename_group(dn: impl Into<String>, current_name: &str) -> Self {
        let value = current_name.to_string();
        Self {
            title: "rename group (cn / RDN)".into(),
            label: "cn".into(),
            cursor: value.len(),
            target: Target::Rename { dn: dn.into(), old_name: value.clone() },
            value,
            masked: false,
            ctx: dctx(),
        }
    }

    /// Prompt for a new domain's suffix (a `dc=…` DN) to create on the server of
    /// session `template_idx`.
    pub fn new_domain(template_idx: usize) -> Self {
        Self {
            title: "create domain (new naming context)".into(),
            label: "suffix (e.g. dc=team,dc=example)".into(),
            value: String::new(),
            cursor: 0,
            masked: false,
            ctx: dctx(),
            target: Target::NewDomain { template_idx },
        }
    }

    /// Prompt for a new schema definition (RFC 4512), pre-filled with a skeleton for
    /// `kind`, to add to the server of session `session_idx`.
    pub fn new_schema(session_idx: usize, kind: crate::ldap::client::SchemaKind) -> Self {
        use crate::ldap::client::SchemaKind;
        let (title, value) = match kind {
            SchemaKind::Attribute => (
                "add attributeType",
                "( 1.3.6.1.4.1.99999.1.1 NAME 'myAttr' DESC '' SYNTAX 1.3.6.1.4.1.1466.115.121.1.15 SINGLE-VALUE )",
            ),
            SchemaKind::ObjectClass => (
                "add objectClass",
                "( 1.3.6.1.4.1.99999.2.1 NAME 'myClass' DESC '' SUP top AUXILIARY MUST () MAY () )",
            ),
        };
        let value = value.to_string();
        Self {
            title: title.into(),
            label: "definition (RFC 4512)".into(),
            cursor: value.len(),
            value,
            masked: false,
            ctx: TextCtx::LTR, // schema definitions are ASCII/LTR
            target: Target::NewSchema { session_idx, kind },
        }
    }

    pub fn handle_key(&mut self, key: KeyCode, _mods: KeyModifiers) -> OverlayResult {
        use KeyCode::*;
        match key {
            Esc => OverlayResult::Cancel,
            Enter => {
                let value = self.value.clone();
                match &self.target {
                    Target::Attr { dn, attr } => OverlayResult::Commit(Action::SetAttr {
                        dn: dn.clone(),
                        attr: attr.clone(),
                        // An empty edit clears the attribute.
                        values: if value.is_empty() { vec![] } else { vec![value] },
                    }),
                    Target::Rename { dn, old_name } => {
                        // An empty or unchanged name is a no-op, not a rename.
                        if value.is_empty() || &value == old_name {
                            OverlayResult::Cancel
                        } else {
                            OverlayResult::Commit(Action::RenameGroup {
                                dn: dn.clone(),
                                new_cn: value,
                                old_name: old_name.clone(),
                            })
                        }
                    }
                    Target::NewDomain { template_idx } => {
                        if value.is_empty() {
                            OverlayResult::Cancel
                        } else {
                            OverlayResult::CreateDomain { template_idx: *template_idx, suffix: value }
                        }
                    }
                    Target::NewSchema { session_idx, kind } => {
                        if value.trim().is_empty() {
                            OverlayResult::Cancel
                        } else {
                            OverlayResult::AddSchema { session_idx: *session_idx, kind: *kind, definition: value }
                        }
                    }
                }
            }
            // Bidi-correct caret motion: Left/Right follow visual order.
            Left => {
                if let Some(c) = visual_step(&self.value, self.cursor, Direction::Left, self.ctx) {
                    self.cursor = c;
                }
                OverlayResult::Stay
            }
            Right => {
                if let Some(c) = visual_step(&self.value, self.cursor, Direction::Right, self.ctx) {
                    self.cursor = c;
                }
                OverlayResult::Stay
            }
            // Everything else (insert/delete/Home/End) is grapheme-aware line editing.
            _ => { line_edit(&mut self.value, &mut self.cursor, key); OverlayResult::Stay }
        }
    }

    /// A pasted value drops into the single-line field verbatim (newlines stripped).
    pub fn handle_paste(&mut self, text: &str) -> OverlayResult {
        super::paste_into(&mut self.value, &mut self.cursor, text, false);
        OverlayResult::Stay
    }

    pub fn render(&self, buf: &mut Buffer, area: Rect) {
        let w = area.width.saturating_sub(8).clamp(20, 72);
        let rect = modal_frame(buf, area, w, 6);
        btxt(buf, rect.x + 2, rect.y, &format!("  {}  ", self.title), s_title());
        btxt(buf, rect.x + 2, rect.y + rect.height - 1, " Enter:save  Esc:cancel ", s_dim());

        // Label line, then the input field below it.
        btxt(buf, rect.x + 2, rect.y + 1, &self.label, s_subhead());
        let field = Rect::new(rect.x + 2, rect.y + 3, rect.width.saturating_sub(4), 1);
        let opts = FieldRender {
            style: s_normal(),
            cursor_style: s_sel(),
            mask: self.masked.then_some('•'),
            ctx: self.ctx,
        };
        let mut scroll = 0;
        render_field(buf, field, &self.value, self.cursor, &mut scroll, &opts);
    }
}
