//! Render an [`Action`] as an RFC 2849 LDIF change record.
//!
//! One serializer feeds two features: the rollback journal (written to disk, so a
//! change is replayable with `ldapmodify`) and the live change-preview tile. Values
//! that aren't LDIF-safe (binary, leading space/`:`/`<`, control chars, non-ASCII)
//! are base64-encoded as `attr:: …`, per the spec.

use crate::schema::Schema;
use super::overlay::Action;

/// The forward LDIF change record for `action`, resolving DNs via `schema`/`base_dn`.
pub fn action_ldif(action: &Action, base_dn: &str, schema: &Schema) -> String {
    match action {
        Action::SetAttr { dn, attr, values } => {
            if values.is_empty() {
                modify(dn, &format!("delete: {attr}"), &[])
            } else {
                let body = str_lines(attr, values);
                modify(dn, &format!("replace: {attr}"), &body)
            }
        }
        Action::SetKeys { dn, keys } => {
            let attr = schema.ssh_key;
            if keys.is_empty() {
                modify(dn, &format!("delete: {attr}"), &[])
            } else {
                modify(dn, &format!("replace: {attr}"), &str_lines(attr, keys))
            }
        }
        Action::AddMember { group_dn, uid, .. } =>
            modify(group_dn, &format!("add: {}", schema.member), &[attr_line(schema.member, uid.as_bytes())]),
        Action::DelMember { group_dn, uid, .. } =>
            modify(group_dn, &format!("delete: {}", schema.member), &[attr_line(schema.member, uid.as_bytes())]),
        Action::SetPasswd { dn, .. } => format!(
            "# dn: {dn}\n# changetype: modify — set userPassword (value withheld; via exop or {{CRYPT}})\n"
        ),
        Action::CreateUser(spec) => {
            let dn = schema.user_dn(&spec.uid, base_dn);
            let mut body: Vec<String> = schema.user_object_classes.iter()
                .map(|oc| attr_line("objectClass", oc.as_bytes()))
                .collect();
            body.push(attr_line(schema.uid, spec.uid.as_bytes()));
            body.push(attr_line(schema.cn, spec.cn.as_bytes()));
            body.push(attr_line(schema.sn, spec.sn.as_bytes()));
            if let Some(g) = &spec.given_name {
                if !g.is_empty() { body.push(attr_line(schema.given_name, g.as_bytes())); }
            }
            body.push(attr_line(schema.uid_number, spec.uid_number.to_string().as_bytes()));
            body.push(attr_line(schema.gid_number, spec.gid_number.to_string().as_bytes()));
            body.push(attr_line(schema.home, spec.home.as_bytes()));
            body.push(attr_line(schema.shell, spec.shell.as_bytes()));
            add(&dn, &body)
        }
        Action::CreateGroup { name, gid_number } => {
            let dn = format!("{}={},{},{}", schema.cn, name, schema.group_ou, base_dn);
            let body = vec![
                attr_line("objectClass", b"top"),
                attr_line("objectClass", b"posixGroup"),
                attr_line(schema.cn, name.as_bytes()),
                attr_line(schema.gid_number, gid_number.to_string().as_bytes()),
            ];
            add(&dn, &body)
        }
        Action::DeleteEntry { dn, .. } | Action::DeleteGroup { dn, .. } =>
            format!("dn: {dn}\nchangetype: delete\n"),
        Action::RenameGroup { dn, new_cn, .. } =>
            format!("dn: {dn}\nchangetype: modrdn\nnewrdn: cn={new_cn}\ndeleteoldrdn: 1\n"),
        Action::RemoveAlias { dn, alias, .. } =>
            modify(dn, "delete: cn", &[attr_line("cn", alias.as_bytes())]),
        Action::AddAlias { dn, alias, .. } =>
            modify(dn, "add: cn", &[attr_line("cn", alias.as_bytes())]),
        Action::RestoreEntry { dn, attrs, .. } => {
            let mut body = Vec::new();
            for (attr, vals) in attrs {
                for v in vals {
                    body.push(attr_line(attr, v));
                }
            }
            add(dn, &body)
        }
    }
}

// ── content records (backup / export) ────────────────────────────────────────

/// An RFC 2849 *content* record (no `changetype`): the DN followed by every attribute
/// value — text attrs from `attrs`, binary ones from `bin` (e.g. `jpegPhoto`) — with
/// anything not LDIF-safe base64-encoded. Attributes are emitted sorted with
/// `objectClass` first, so an export is stable and re-loadable with `ldapadd`.
pub fn entry_ldif(
    dn: &str,
    attrs: &std::collections::HashMap<String, Vec<String>>,
    bin: &std::collections::HashMap<String, Vec<Vec<u8>>>,
) -> String {
    let mut s = format!("{}\n", attr_line("dn", dn.as_bytes()));
    let mut keys: Vec<&String> = attrs.keys().chain(bin.keys()).collect();
    keys.sort_by(|a, b| {
        // objectClass first, then case-insensitive alpha.
        let rank = |k: &str| !k.eq_ignore_ascii_case("objectClass");
        rank(a).cmp(&rank(b)).then_with(|| a.to_lowercase().cmp(&b.to_lowercase()))
    });
    keys.dedup();
    for k in keys {
        if let Some(vals) = attrs.get(k) {
            for v in vals { s.push_str(&attr_line(k, v.as_bytes())); s.push('\n'); }
        }
        if let Some(vals) = bin.get(k) {
            for v in vals { s.push_str(&attr_line(k, v)); s.push('\n'); }
        }
    }
    s
}

/// An RFC 2849 content record straight from raw attribute bytes (as
/// [`LdapClient::read_entry_raw`](crate::ldap::client::LdapClient::read_entry_raw)
/// returns them) — for a dry-run migration preview.
pub fn entry_ldif_raw(dn: &str, attrs: &[(String, Vec<Vec<u8>>)]) -> String {
    let mut s = format!("{}\n", attr_line("dn", dn.as_bytes()));
    for (attr, vals) in attrs {
        for v in vals {
            s.push_str(&attr_line(attr, v));
            s.push('\n');
        }
    }
    s
}

// ── record shapes ────────────────────────────────────────────────────────────

fn modify(dn: &str, op: &str, body: &[String]) -> String {
    let mut s = format!("dn: {dn}\nchangetype: modify\n{op}\n");
    for line in body { s.push_str(line); s.push('\n'); }
    s.push_str("-\n");
    s
}

fn add(dn: &str, body: &[String]) -> String {
    let mut s = format!("dn: {dn}\nchangetype: add\n");
    for line in body { s.push_str(line); s.push('\n'); }
    s
}

fn str_lines(attr: &str, values: &[String]) -> Vec<String> {
    values.iter().map(|v| attr_line(attr, v.as_bytes())).collect()
}

// ── value encoding ───────────────────────────────────────────────────────────

/// One `attr: value` line, base64-encoding (`attr:: …`) when the value isn't
/// LDIF-safe (RFC 2849 §2).
fn attr_line(attr: &str, value: &[u8]) -> String {
    if needs_base64(value) {
        format!("{attr}:: {}", base64(value))
    } else {
        // Safe by construction: value is valid ASCII text here.
        format!("{attr}: {}", std::str::from_utf8(value).unwrap_or_default())
    }
}

fn needs_base64(value: &[u8]) -> bool {
    if let Some(&b) = value.first() {
        if b == b' ' || b == b':' || b == b'<' {
            return true;
        }
    }
    value.iter().any(|&b| !(0x20..=0x7e).contains(&b))
}

/// Standard base64 (RFC 4648) — small, so census avoids a dependency for it.
fn base64(data: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(A[(n >> 18 & 63) as usize] as char);
        out.push(A[(n >> 12 & 63) as usize] as char);
        out.push(if chunk.len() > 1 { A[(n >> 6 & 63) as usize] as char } else { '=' });
        out.push(if chunk.len() > 2 { A[(n & 63) as usize] as char } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_known_vectors() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn add_member_is_a_modify_record() {
        let s = Schema::rfc2307();
        let a = Action::AddMember {
            group_dn: "cn=cobalt,ou=groups,dc=lofar,dc=eu".into(),
            uid: "quixote".into(),
            group: "cobalt".into(),
        };
        let ldif = action_ldif(&a, "dc=lofar,dc=eu", &s);
        assert_eq!(
            ldif,
            "dn: cn=cobalt,ou=groups,dc=lofar,dc=eu\nchangetype: modify\nadd: memberUid\nmemberUid: quixote\n-\n"
        );
    }

    #[test]
    fn entry_ldif_is_a_content_record_objectclass_first() {
        use std::collections::HashMap;
        let mut attrs = HashMap::new();
        attrs.insert("uid".to_string(), vec!["ada".to_string()]);
        attrs.insert("objectClass".to_string(), vec!["top".to_string(), "posixAccount".to_string()]);
        attrs.insert("cn".to_string(), vec!["Ada".to_string()]);
        let s = entry_ldif("uid=ada,dc=x", &attrs, &HashMap::new());
        // dn first, then objectClass (both values), then the rest alphabetically.
        assert!(s.starts_with(
            "dn: uid=ada,dc=x\nobjectClass: top\nobjectClass: posixAccount\ncn: Ada\n"
        ), "got:\n{s}");
        assert!(!s.contains("changetype")); // a content record, not a change record
    }

    #[test]
    fn entry_ldif_base64s_binary_attrs() {
        use std::collections::HashMap;
        let mut bin = HashMap::new();
        bin.insert("jpegPhoto".to_string(), vec![vec![0xff, 0xd8, 0xff]]);
        let s = entry_ldif("uid=x,dc=y", &HashMap::new(), &bin);
        assert!(s.contains("jpegPhoto:: "), "got:\n{s}");
    }

    #[test]
    fn non_ascii_value_is_base64() {
        let line = attr_line("cn", "café".as_bytes());
        assert!(line.starts_with("cn:: "), "got {line:?}");
    }

    #[test]
    fn rename_group_is_a_modrdn_record() {
        let s = Schema::rfc2307();
        let a = Action::RenameGroup {
            dn: "cn=knights-errant,ou=groups,dc=lofar,dc=eu".into(),
            new_cn: "knights".into(),
            old_name: "knights-errant".into(),
        };
        let ldif = action_ldif(&a, "dc=lofar,dc=eu", &s);
        assert_eq!(
            ldif,
            "dn: cn=knights-errant,ou=groups,dc=lofar,dc=eu\nchangetype: modrdn\nnewrdn: cn=knights\ndeleteoldrdn: 1\n"
        );
    }
}
