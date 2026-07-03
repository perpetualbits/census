//! A connected directory source: an [`LdapClient`] plus its cached users/groups
//! and derived membership index.
//!
//! The app holds a `Vec<Session>` with an `active` index. Today there is always
//! exactly one, but this is the seam that lets census point at several LDAP
//! sources at once (and migrate between them) without reworking call sites.

use crate::config::Config;
use crate::conninfo::{ConnInfo, PwSource};
use crate::ldap::client::{Group, LdapClient, User};

pub struct Session {
    /// Human-readable source label (shown in a future source switcher).
    #[allow(dead_code)] // surfaced by the multi-source switcher (P9+)
    pub label: String,
    pub client: LdapClient,
    pub users: Vec<User>,
    pub groups: Vec<Group>,
    /// `true` when the user/group load hit the server size cap (more exist than loaded).
    pub users_truncated: bool,
    pub groups_truncated: bool,
    /// How this session reached the directory + got its password (for the UI gap).
    pub conn: ConnInfo,
}

impl Session {
    /// Connect, bind, and load the initial user/group caches.
    pub fn connect(cfg: &Config, password: Option<&str>, pw_source: PwSource, label: String)
        -> anyhow::Result<Self>
    {
        let mut client = LdapClient::connect(cfg, password)?;
        let tls = if cfg.server.use_ssl { "LDAPS" }
                  else if cfg.server.start_tls { "STARTTLS" } else { "LDAP" };
        let conn = ConnInfo { via: client.conn_via().clone(), tls, password: pw_source };
        let (users, users_truncated) = client.list_users()?;
        let (groups, groups_truncated) = client.list_groups()?;
        Ok(Self { label, client, users, groups, users_truncated, groups_truncated, conn })
    }

    /// Re-read the user list from the directory.
    pub fn refresh_users(&mut self) -> anyhow::Result<()> {
        let (users, truncated) = self.client.list_users()?;
        self.users = users;
        self.users_truncated = truncated;
        Ok(())
    }

    /// Re-read the group list from the directory.
    pub fn refresh_groups(&mut self) -> anyhow::Result<()> {
        let (groups, truncated) = self.client.list_groups()?;
        self.groups = groups;
        self.groups_truncated = truncated;
        Ok(())
    }

    /// Unbind the connection.
    pub fn close(self) -> anyhow::Result<()> {
        self.client.close()
    }
}
