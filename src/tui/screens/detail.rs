//! Per-user detail pane: full attribute record, group memberships, SSH keys.
//!
//! When the pane is focused, a cursor selects one *editable* attribute (see
//! [`EDITABLE`]); the app reads [`selected_target`] to drive attribute editing.

use mullion::{
    label::Align,
    table::{ColumnDef, ColumnGrid, ColumnKind},
    Buffer, Rect,
};

use crate::ldap::client::User;
use crate::tui::app::App;
use crate::tui::draw::{btxt, fill_row, hline};
use crate::tui::theme::*;

/// One rendered line in the (scrollable) detail body.
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

/// Attributes shown first, in this order, when present.
const PRIMARY: &[&str] = &[
    "uid", "cn", "sn", "givenName", "uidNumber", "gidNumber",
    "homeDirectory", "loginShell", "mail", "telephoneNumber",
];
/// Rendered in their own sections rather than the generic attribute list.
const HIDDEN: &[&str] = &["sshPublicKey", "objectClass", "userPassword"];
/// Attributes the cursor may select and edit (single-valued, admin-safe).
const EDITABLE: &[&str] = &[
    "cn", "sn", "givenName", "displayName", "mail", "telephoneNumber",
    "loginShell", "homeDirectory", "gidNumber",
];

pub fn render(app: &App, buf: &mut Buffer, area: Rect, focused: bool) {
    if area.width < 12 || area.height < 3 { return; }

    let head = if focused { s_head() } else { s_subhead() };
    let Some(user) = app.detail() else {
        ColumnGrid::write_text(buf, area, area.y, "(no user selected)", Align::Start, s_dim());
        return;
    };

    let (rows, targets) = model(user, &app.groups_of(&user.uid));
    let sel_row = (focused && !targets.is_empty())
        .then(|| targets.get(app.detail_cur).map(|t| t.row))
        .flatten();

    ColumnGrid::write_text(buf, area, area.y, "detail", Align::Start, head);
    hline(buf, Rect::new(area.x, area.y + 1, area.width, 1));

    let body = Rect::new(area.x, area.y + 2, area.width, area.height.saturating_sub(2));
    let vis  = body.height as usize;
    let off  = app.detail_scroll.min(rows.len().saturating_sub(1));

    let grid = kv_grid();
    let cols = grid.resolve(body);

    for (i, row) in rows.iter().enumerate().skip(off).take(vis) {
        let y   = body.y + (i - off) as u16;
        let sel = Some(i) == sel_row;
        if sel { fill_row(buf, body.x, y, body.width, s_sel()); }
        match row {
            Row::Title(t)   => btxt(buf, body.x, y, t, if sel { s_sel() } else { s_title() }),
            Row::Section(t) => btxt(buf, body.x, y, t, head),
            Row::Kv(k, v) => {
                let (ks, vs) = if sel { (s_sel(), s_sel()) } else { (s_dim(), s_normal()) };
                ColumnGrid::write_text(buf, cols[0], y, k, Align::Start, ks);
                ColumnGrid::write_text(buf, cols[2], y, v, Align::Start, vs);
            }
            Row::Text(t) => btxt(buf, body.x + 2, y, t, if sel { s_sel() } else { s_normal() }),
            Row::Blank => {}
        }
    }

    if rows.len() > vis {
        let more = format!("… {}/{} ", off + vis.min(rows.len() - off), rows.len());
        let mx = area.x + area.width.saturating_sub(more.len() as u16);
        btxt(buf, mx, area.y, &more, s_dim());
    }
}

/// Build the row model + edit targets for the currently-selected user.
fn build(app: &App) -> (Vec<Row>, Vec<EditTarget>) {
    match app.detail() {
        Some(user) => model(user, &app.groups_of(&user.uid)),
        None => (Vec::new(), Vec::new()),
    }
}

/// Editable targets for the currently-selected user (cursor order = render order).
pub fn edit_targets(app: &App) -> Vec<EditTarget> {
    build(app).1
}

/// The row index of the `n`-th edit target (for keeping the cursor in view).
pub fn target_row(app: &App, idx: usize) -> Option<usize> {
    edit_targets(app).get(idx).map(|t| t.row)
}

/// Total scrollable rows for the currently-selected user (for scroll clamping).
pub fn row_count(app: &App) -> usize {
    build(app).0.len()
}

fn model(user: &User, group_names: &[String]) -> (Vec<Row>, Vec<EditTarget>) {
    let mut rows = Vec::new();
    let mut targets = Vec::new();

    let title = if user.cn.is_empty() {
        user.uid.clone()
    } else {
        format!("{} — {}", user.uid, user.cn)
    };
    rows.push(Row::Title(title));
    rows.push(Row::Blank);

    let push_kv = |rows: &mut Vec<Row>, targets: &mut Vec<EditTarget>, name: &str, val: &str| {
        if EDITABLE.contains(&name) {
            targets.push(EditTarget { row: rows.len(), attr: name.to_string(), value: val.to_string() });
        }
        rows.push(Row::Kv(name.to_string(), val.to_string()));
    };

    // Primary attributes first, in declared order.
    for &name in PRIMARY {
        if let Some(vals) = user.attrs.get(name) {
            for v in vals {
                push_kv(&mut rows, &mut targets, name, v);
            }
        }
    }

    // Remaining attributes, alphabetically.
    let mut others: Vec<&String> = user.attrs.keys()
        .filter(|k| !PRIMARY.contains(&k.as_str()) && !HIDDEN.contains(&k.as_str()))
        .collect();
    others.sort();
    for k in others {
        if let Some(vals) = user.attrs.get(k) {
            for v in vals {
                push_kv(&mut rows, &mut targets, k, v);
            }
        }
    }

    // Group memberships.
    rows.push(Row::Blank);
    rows.push(Row::Section(format!("groups ({})", group_names.len())));
    if group_names.is_empty() {
        rows.push(Row::Text("(none)".into()));
    } else {
        rows.push(Row::Text(group_names.join(", ")));
    }

    // SSH keys.
    rows.push(Row::Blank);
    rows.push(Row::Section(format!("ssh keys ({})", user.ssh_keys.len())));
    if user.ssh_keys.is_empty() {
        rows.push(Row::Text("(none)".into()));
    } else {
        for key in &user.ssh_keys {
            rows.push(Row::Text(summarize_key(key)));
        }
    }

    (rows, targets)
}

/// Condense an OpenSSH public key line to `type …tail comment`.
fn summarize_key(key: &str) -> String {
    let mut parts = key.split_whitespace();
    let kind = parts.next().unwrap_or("key");
    let blob = parts.next().unwrap_or("");
    let comment = parts.next().unwrap_or("");
    let tail: String = blob.chars().rev().take(8).collect::<String>().chars().rev().collect();
    if comment.is_empty() {
        format!("{kind} …{tail}")
    } else {
        format!("{kind} …{tail}  {comment}")
    }
}

fn kv_grid() -> ColumnGrid {
    ColumnGrid::new(vec![
        ColumnDef::fixed(15, ColumnKind::Text),             // attr name
        ColumnDef::fixed(1,  ColumnKind::Custom),           // gap
        ColumnDef::fill(1,   ColumnKind::Text).with_min(8), // value
    ])
}
