//! Per-group detail pane: full attribute record + members, with in-place editing.
//!
//! The group analogue of [`super::detail`]. When focused, a cursor selects one
//! editable attribute (see [`EDITABLE`]/[`ADDABLE`]); the app reads
//! [`edit_targets`] to drive attribute editing. The `cn` (RDN) is shown but not an
//! edit target — renaming is a separate `modrdn` op (`r`); aliases are removed with `a`.

use mullion::{
    label::Align,
    table::{ColumnDef, ColumnGrid, ColumnKind},
    Buffer, Rect,
};

use crate::ldap::client::Group;
use crate::tui::app::App;
use crate::tui::draw::{btxt, fill_row, hline, vscroll};
use crate::tui::theme::*;

enum Row {
    Title(String),
    Section(String),
    Kv(String, String),
    Text(String),
    Blank,
}

/// An editable attribute the detail cursor can land on.
#[derive(Clone)]
pub struct EditTarget {
    row: usize,
    pub attr: String,
    pub value: String,
}

/// Attributes the cursor may edit in place (single-valued, admin-safe).
const EDITABLE: &[&str] = &["gidNumber", "description"];
/// Known attributes offered for adding when absent (rendered as an empty edit slot).
const ADDABLE: &[&str] = &["description"];
/// Shown in their own rows/sections rather than the generic attribute list.
const HIDDEN: &[&str] = &["cn", "memberUid", "objectClass"];

pub fn render(app: &App, buf: &mut Buffer, area: Rect, focused: bool) {
    if area.width < 12 || area.height < 3 {
        return;
    }
    let head = if focused { s_head() } else { s_subhead() };
    let Some(group) = app.groups().get(app.groups_cur.cursor) else {
        ColumnGrid::write_text(buf, area, area.y, "(no group selected)", Align::Start, s_dim());
        return;
    };

    let (rows, targets) = model(group);
    let sel_row = (focused && !targets.is_empty())
        .then(|| targets.get(app.group_detail_cur).map(|t| t.row))
        .flatten();

    ColumnGrid::write_text(buf, area, area.y, "group detail", Align::Start, head);
    hline(buf, Rect::new(area.x, area.y + 1, area.width, 1));

    let body = Rect::new(area.x, area.y + 2, area.width, area.height.saturating_sub(2));
    let vis = body.height as usize;
    let off = app.group_detail_scroll.min(rows.len().saturating_sub(1));
    let content = vscroll(buf, body, off, rows.len(), vis);

    let grid = kv_grid();
    let cols = grid.resolve(content);

    for (i, row) in rows.iter().enumerate().skip(off).take(vis) {
        let y = content.y + (i - off) as u16;
        let sel = Some(i) == sel_row;
        if sel { fill_row(buf, content.x, y, content.width, s_sel()); }
        match row {
            Row::Title(t)   => btxt(buf, content.x, y, t, if sel { s_sel() } else { s_title() }),
            Row::Section(t) => btxt(buf, content.x, y, t, head),
            Row::Kv(k, v) => {
                let (ks, vs) = if sel { (s_sel(), s_sel()) } else { (s_dim(), s_normal()) };
                ColumnGrid::write_text(buf, cols[0], y, k, Align::Start, ks);
                ColumnGrid::write_text_ctx(buf, cols[2], y, v, Align::Start, vs, dctx());
            }
            Row::Text(t) => btxt(buf, content.x + 2, y, t, if sel { s_sel() } else { s_normal() }),
            Row::Blank => {}
        }
    }
}

fn model(group: &Group) -> (Vec<Row>, Vec<EditTarget>) {
    let mut rows = Vec::new();
    let mut targets = Vec::new();

    let gid_disp = group.gid_number.map(|n| n.to_string()).unwrap_or_else(|| "—".into());
    rows.push(Row::Title(format!("{}  (gid {})", group.name, gid_disp)));
    rows.push(Row::Blank);

    // Identity: the RDN cn (read-only) and any alias cn values.
    rows.push(Row::Kv("cn".into(), group.name.clone()));
    for a in &group.aliases {
        rows.push(Row::Kv("cn (alias)".into(), a.clone()));
    }

    let push_editable = |rows: &mut Vec<Row>, targets: &mut Vec<EditTarget>, attr: &str, val: String| {
        targets.push(EditTarget { row: rows.len(), attr: attr.to_string(), value: val.clone() });
        let shown = if val.is_empty() { "(none — press e to add)".to_string() } else { val };
        rows.push(Row::Kv(attr.to_string(), shown));
    };

    // Editable primary attributes: present value, or an empty add-slot for addables.
    let gid_raw = group.attrs.get("gidNumber").and_then(|v| v.first()).cloned().unwrap_or(gid_disp);
    push_editable(&mut rows, &mut targets, "gidNumber", gid_raw);
    let desc = group.attrs.get("description").and_then(|v| v.first()).cloned();
    if desc.is_some() || ADDABLE.contains(&"description") {
        push_editable(&mut rows, &mut targets, "description", desc.unwrap_or_default());
    }

    // Remaining attributes, alphabetical, read-only.
    let mut others: Vec<&String> = group.attrs.keys()
        .filter(|k| !HIDDEN.contains(&k.as_str()) && !EDITABLE.contains(&k.as_str()))
        .collect();
    others.sort();
    for k in others {
        if let Some(vals) = group.attrs.get(k) {
            for v in vals { rows.push(Row::Kv(k.clone(), v.clone())); }
        }
    }

    if let Some(ocs) = group.attrs.get("objectClass") {
        rows.push(Row::Blank);
        rows.push(Row::Section(format!("objectClass ({})", ocs.len())));
        rows.push(Row::Text(ocs.join(", ")));
    }

    rows.push(Row::Blank);
    rows.push(Row::Section(format!("members ({})", group.members.len())));
    if group.members.is_empty() {
        rows.push(Row::Text("(none)".into()));
    } else {
        for m in &group.members {
            rows.push(Row::Text(m.clone()));
        }
    }

    (rows, targets)
}

fn build(app: &App) -> (Vec<Row>, Vec<EditTarget>) {
    match app.groups().get(app.groups_cur.cursor) {
        Some(g) => model(g),
        None => (Vec::new(), Vec::new()),
    }
}

/// Editable targets for the cursored group (cursor order = render order).
pub fn edit_targets(app: &App) -> Vec<EditTarget> { build(app).1 }

/// The row index of the `n`-th edit target (for keeping the cursor in view).
pub fn target_row(app: &App, idx: usize) -> Option<usize> {
    edit_targets(app).get(idx).map(|t| t.row)
}

/// Total scrollable rows for the cursored group (for scroll clamping).
pub fn row_count(app: &App) -> usize { build(app).0.len() }

fn kv_grid() -> ColumnGrid {
    ColumnGrid::new(vec![
        ColumnDef::fixed(15, ColumnKind::Text),
        ColumnDef::fixed(1,  ColumnKind::Custom),
        ColumnDef::fill(1,   ColumnKind::Text).with_min(8),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn grp() -> Group {
        let mut attrs = HashMap::new();
        attrs.insert("gidNumber".to_string(), vec!["10010".to_string()]);
        attrs.insert("objectClass".to_string(), vec!["top".to_string(), "posixGroup".to_string()]);
        Group {
            dn: "cn=windmillfighters,ou=groups,dc=lofar,dc=eu".into(),
            name: "windmillfighters".into(),
            aliases: vec![],
            gid_number: Some(10010),
            members: vec!["quixote".into()],
            dup_name: false,
            dup_gid: false,
            attrs,
        }
    }

    #[test]
    fn editable_targets_are_gid_and_description_not_the_rdn() {
        let (_rows, targets) = model(&grp());
        let attrs: Vec<&str> = targets.iter().map(|t| t.attr.as_str()).collect();
        assert!(attrs.contains(&"gidNumber"), "gidNumber editable: {attrs:?}");
        // description is offered even when absent (it's an addable).
        assert!(attrs.contains(&"description"), "description offered: {attrs:?}");
        // The cn RDN is shown but never an edit target (rename is a separate op).
        assert!(!attrs.contains(&"cn"), "cn must not be editable: {attrs:?}");
    }
}
