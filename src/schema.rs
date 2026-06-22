//! Directory schema descriptor.
//!
//! Holds the attribute names, object classes, and container layout for one kind
//! of directory, so the rest of the code never hard-codes (say) `uid` or
//! `posixAccount`. Today only [`Schema::rfc2307`] exists; Active Directory and
//! eDirectory descriptors plus an attribute mapper slot in here later without
//! touching the LDAP or TUI layers.

/// Attribute names, object classes, and container layout for a directory.
///
/// Fields are consumed progressively as the write features land (user creation
/// reads `user_object_classes`, the SSH-key view reads `ssh_object_class`, …).
#[derive(Debug, Clone)]
#[allow(dead_code)] // descriptor: some fields are wired up in later phases
pub struct Schema {
    /// RDN attribute of a user entry, e.g. `uid`.
    pub user_rdn: &'static str,
    /// User container relative to the base DN, e.g. `ou=users`.
    pub user_ou: &'static str,
    /// Group container relative to the base DN, e.g. `ou=groups`.
    pub group_ou: &'static str,
    /// Search filter selecting user entries.
    pub user_filter: &'static str,
    /// Search filter selecting group entries.
    pub group_filter: &'static str,

    pub uid: &'static str,
    pub cn: &'static str,
    pub sn: &'static str,
    pub given_name: &'static str,
    pub uid_number: &'static str,
    pub gid_number: &'static str,
    pub home: &'static str,
    pub shell: &'static str,
    pub ssh_key: &'static str,
    /// Group membership attribute (bare UIDs for RFC 2307 `memberUid`).
    pub member: &'static str,

    /// Object classes set on a freshly created user.
    pub user_object_classes: &'static [&'static str],
    /// Object class required before `ssh_key` may be stored.
    pub ssh_object_class: &'static str,
}

impl Schema {
    /// RFC 2307 / POSIX schema as used by the LOFAR OpenLDAP directory.
    pub fn rfc2307() -> Self {
        Self {
            user_rdn: "uid",
            user_ou: "ou=users",
            group_ou: "ou=groups",
            user_filter: "(objectClass=posixAccount)",
            group_filter: "(objectClass=posixGroup)",
            uid: "uid",
            cn: "cn",
            sn: "sn",
            given_name: "givenName",
            uid_number: "uidNumber",
            gid_number: "gidNumber",
            home: "homeDirectory",
            shell: "loginShell",
            ssh_key: "sshPublicKey",
            member: "memberUid",
            user_object_classes: &["top", "posixAccount", "inetOrgPerson", "shadowAccount"],
            ssh_object_class: "ldapPublicKey",
        }
    }

    /// DN of the user container under `base_dn`.
    pub fn user_base(&self, base_dn: &str) -> String {
        format!("{},{}", self.user_ou, base_dn)
    }

    /// DN of the group container under `base_dn`.
    pub fn group_base(&self, base_dn: &str) -> String {
        format!("{},{}", self.group_ou, base_dn)
    }

    /// DN of a single user with the given RDN value under `base_dn`.
    #[allow(dead_code)] // wired up by user create/delete (P8)
    pub fn user_dn(&self, rdn_value: &str, base_dn: &str) -> String {
        format!("{}={},{},{}", self.user_rdn, rdn_value, self.user_ou, base_dn)
    }
}
