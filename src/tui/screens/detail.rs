//! Per-user detail pane: full attribute record, group memberships, SSH keys.

use mullion::{
    label::Align,
    table::{ColumnDef, ColumnGrid, ColumnKind},
    Buffer, Rect,
};

use crate::ldap::client::User;
use crate::tui::app::App;
use crate::tui::draw::{btxt, hline};
use crate::tui::theme::*;

/// One rendered line in the (scrollable) detail body.
enum Row {
    Title(String),
    Section(String),
    Kv(String, String),
    Text(String),
    Blank,
}

/// Attributes shown first, in this order, when present. Everything else follows
/// alphabetically. `sshPublicKey` and `objectClass` are rendered in their own
/// sections, so they are excluded from the generic attribute list.
const PRIMARY: &[&str] = &[
    "uid", "cn", "sn", "givenName", "uidNumber", "gidNumber",
    "homeDirectory", "loginShell", "mail", "telephoneNumber",
];
const HIDDEN: &[&str] = &["sshPublicKey", "objectClass", "userPassword"];

pub fn render(app: &App, buf: &mut Buffer, area: Rect, focused: bool) {
    if area.width < 12 || area.height < 3 { return; }

    let head = if focused { s_head() } else { s_subhead() };
    let Some(user) = app.detail() else {
        ColumnGrid::write_text(buf, area, area.y, "(no user selected)", Align::Start, s_dim());
        return;
    };

    let rows = build_rows(app, user);

    // Header line is fixed; body below it scrolls.
    ColumnGrid::write_text(buf, area, area.y, "detail", Align::Start, head);
    hline(buf, Rect::new(area.x, area.y + 1, area.width, 1));

    let body = Rect::new(area.x, area.y + 2, area.width, area.height.saturating_sub(2));
    let vis  = body.height as usize;
    let off  = app.detail_scroll.min(rows.len().saturating_sub(1));

    let grid = kv_grid();
    let cols = grid.resolve(body);

    for (i, row) in rows.iter().enumerate().skip(off).take(vis) {
        let y = body.y + (i - off) as u16;
        match row {
            Row::Title(t)   => btxt(buf, body.x, y, t, s_title()),
            Row::Section(t) => btxt(buf, body.x, y, t, head),
            Row::Kv(k, v) => {
                ColumnGrid::write_text(buf, cols[0], y, k, Align::Start, s_dim());
                ColumnGrid::write_text(buf, cols[2], y, v, Align::Start, s_normal());
            }
            Row::Text(t) => btxt(buf, body.x + 2, y, t, s_normal()),
            Row::Blank => {}
        }
    }

    // Scroll affordance.
    if rows.len() > vis {
        let more = format!("… {}/{} ", off + vis.min(rows.len() - off), rows.len());
        let mx = area.x + area.width.saturating_sub(more.len() as u16);
        btxt(buf, mx, area.y, &more, s_dim());
    }
}

/// Total scrollable rows for the currently-selected user (for scroll clamping).
pub fn row_count(app: &App) -> usize {
    match app.detail() {
        Some(user) => build_rows(app, user).len(),
        None => 0,
    }
}

fn build_rows(app: &App, user: &User) -> Vec<Row> {
    let mut rows = Vec::new();

    let title = if user.cn.is_empty() {
        user.uid.clone()
    } else {
        format!("{} — {}", user.uid, user.cn)
    };
    rows.push(Row::Title(title));
    rows.push(Row::Blank);

    // Primary attributes first, in declared order.
    for &name in PRIMARY {
        if let Some(vals) = user.attrs.get(name) {
            for v in vals {
                rows.push(Row::Kv(name.to_string(), v.clone()));
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
                rows.push(Row::Kv(k.clone(), v.clone()));
            }
        }
    }

    // Group memberships (computed against the active session's groups).
    let groups = app.groups_of(&user.uid);
    rows.push(Row::Blank);
    rows.push(Row::Section(format!("groups ({})", groups.len())));
    if groups.is_empty() {
        rows.push(Row::Text("(none)".into()));
    } else {
        rows.push(Row::Text(groups.join(", ")));
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

    rows
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
