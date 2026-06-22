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
}

impl Session {
    /// Connect, bind, and load the initial user/group caches.
    pub fn connect(cfg: &Config, password: Option<&str>, label: String) -> anyhow::Result<Self> {
        let mut client = LdapClient::connect(cfg, password)?;
        let users = client.list_users()?;
        let groups = client.list_groups()?;
        Ok(Self { label, client, users, groups })
    }

    /// Re-read the user list from the directory.
    #[allow(dead_code)] // wired up after the first user-mutating write (P4)
    pub fn refresh_users(&mut self) -> anyhow::Result<()> {
        self.users = self.client.list_users()?;
        Ok(())
    }

    /// Re-read the group list from the directory.
    pub fn refresh_groups(&mut self) -> anyhow::Result<()> {
        self.groups = self.client.list_groups()?;
        Ok(())
    }

    /// Unbind the connection.
    pub fn close(self) -> anyhow::Result<()> {
        self.client.close()
    }
}
