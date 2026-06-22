//! A connected directory source: an [`LdapClient`] plus its cached users/groups
//! and derived membership index.
//!
//! The app holds a `Vec<Session>` with an `active` index. Today there is always
//! exactly one, but this is the seam that lets census point at several LDAP
//! sources at once (and migrate between them) without reworking call sites.

use crate::config::Config;
use crate::ldap::client::{Group, LdapClient, User};

pub struct Session {
    /// Human-readable source label (shown in a future source switcher).
    #[allow(dead_code)] // surfaced by the multi-source switcher (P9+)
    pub label: String,
    pub client: LdapClient,
    pub users: Vec<User>,
    pub groups: Vec<Group>,
    /// Per-user index into `users`: the group names each user belongs to.
    pub user_groups: Vec<Vec<String>>,
}

impl Session {
    /// Connect, bind, and load the initial user/group caches.
    pub fn connect(cfg: &Config, password: Option<&str>, label: String) -> anyhow::Result<Self> {
        let mut client = LdapClient::connect(cfg, password)?;
        let users = client.list_users()?;
        let groups = client.list_groups()?;
        let user_groups = build_user_groups(&users, &groups);
        Ok(Self { label, client, users, groups, user_groups })
    }

    /// Re-read the user list and rebuild the membership index.
    #[allow(dead_code)] // wired up after the first user-mutating write (P4)
    pub fn refresh_users(&mut self) -> anyhow::Result<()> {
        self.users = self.client.list_users()?;
        self.rebuild_user_groups();
        Ok(())
    }

    /// Re-read the group list and rebuild the membership index.
    pub fn refresh_groups(&mut self) -> anyhow::Result<()> {
        self.groups = self.client.list_groups()?;
        self.rebuild_user_groups();
        Ok(())
    }

    fn rebuild_user_groups(&mut self) {
        self.user_groups = build_user_groups(&self.users, &self.groups);
    }

    /// Unbind the connection.
    pub fn close(self) -> anyhow::Result<()> {
        self.client.close()
    }
}

/// Compute, for each user, the names of the groups they are a member of.
pub fn build_user_groups(users: &[User], groups: &[Group]) -> Vec<Vec<String>> {
    users.iter().map(|u| {
        groups.iter()
            .filter(|g| g.members.iter().any(|m| m == &u.uid))
            .map(|g| g.name.clone())
            .collect()
    }).collect()
}
