//! Output formatting for the CLI: line-oriented text by default (greppable), or
//! `--json` for structured output. Data goes to stdout; notes/warnings to stderr.

use crate::ldap::client::{DitNode, Group, User};
use crate::tui::app::SearchHit;

fn json_line<T: serde::Serialize>(value: &T) {
    match serde_json::to_string_pretty(value) {
        Ok(s) => println!("{s}"),
        Err(e) => eprintln!("error: JSON encode failed: {e}"),
    }
}

fn capped_note(capped: bool) {
    if capped {
        eprintln!("NOTE: capped at the browse size limit — more entries exist than shown");
    }
}

pub fn users_list(users: &[User], long: bool, json: bool, capped: bool) {
    capped_note(capped);
    if json {
        json_line(&users);
        return;
    }
    for u in users {
        if long {
            println!(
                "{}\t{}\t{}\t{}\t{}\t{}",
                u.uid, u.cn, u.uid_number, u.gid_number, u.home, u.shell
            );
        } else {
            println!("{}\t{}\t{}\t{}", u.uid, u.cn, u.uid_number, u.gid_number);
        }
    }
}

pub fn user(u: &User, json: bool) {
    if json {
        json_line(u);
        return;
    }
    attr_map(&u.dn, &u.attrs);
}

pub fn groups_list(groups: &[Group], json: bool, capped: bool) {
    capped_note(capped);
    if json {
        json_line(&groups);
        return;
    }
    for g in groups {
        let gid = g.gid_number.map(|n| n.to_string()).unwrap_or_else(|| "-".into());
        println!("{}\t{}\t{}", g.name, gid, g.members.len());
    }
}

pub fn group(g: &Group, json: bool) {
    if json {
        json_line(g);
        return;
    }
    attr_map(&g.dn, &g.attrs);
}

/// A flat list of strings (members, memberships, keys): one per line, or a JSON array.
pub fn string_list(items: &[&str], json: bool) {
    if json {
        json_line(&items);
        return;
    }
    for item in items {
        println!("{item}");
    }
}

pub fn hits(hits: &[SearchHit], json: bool) {
    if json {
        json_line(&hits);
        return;
    }
    for h in hits {
        let kind = match h.kind {
            crate::tui::app::HitKind::User => "user",
            crate::tui::app::HitKind::Group => "group",
        };
        println!("{}\t{}\t{}\t{}", kind, h.key, h.primary, h.secondary);
    }
}

pub fn children(nodes: &[DitNode], json: bool, capped: bool) {
    capped_note(capped);
    if json {
        json_line(&nodes);
        return;
    }
    for n in nodes {
        println!("{}", n.dn);
    }
}

/// An entry's attributes (already display strings), addressed by DN.
pub fn attrs(dn: Option<&str>, attrs: &[(String, Vec<String>)], json: bool) {
    if json {
        // Serialize as { dn, attrs: { name: [values] } }.
        let map: std::collections::BTreeMap<&str, &Vec<String>> =
            attrs.iter().map(|(k, v)| (k.as_str(), v)).collect();
        json_line(&serde_json::json!({ "dn": dn, "attrs": map }));
        return;
    }
    if let Some(d) = dn {
        println!("dn: {d}");
    }
    for (name, values) in attrs {
        for v in values {
            println!("{name}: {v}");
        }
    }
}

/// Print a `HashMap` attribute record (from a parsed `User`/`Group`) as sorted
/// `attr: value` lines under its DN.
fn attr_map(dn: &str, map: &std::collections::HashMap<String, Vec<String>>) {
    println!("dn: {dn}");
    let mut names: Vec<&String> = map.keys().collect();
    names.sort();
    for name in names {
        for v in &map[name] {
            println!("{name}: {v}");
        }
    }
}
