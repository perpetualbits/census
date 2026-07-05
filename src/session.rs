//! A connected directory source: an [`LdapClient`] plus its cached users/groups
//! and derived membership index.
//!
//! The app holds a `Vec<Session>` with an `active` index. Today there is always
//! exactly one, but this is the seam that lets census point at several LDAP
//! sources at once (and migrate between them) without reworking call sites.

use crate::config::{ConnMode, Config};
use crate::conninfo::{ConnInfo, PwSource};
use crate::ldap::client::{Caps, Group, LdapClient, User};

pub struct Session {
    pub client: LdapClient,
    /// Paging controls the server advertises (SSS/VLV unlock windowed browsing).
    pub caps: Caps,
    pub users: Vec<User>,
    pub groups: Vec<Group>,
    /// `true` when the user/group load hit the server size cap (more exist than loaded).
    pub users_truncated: bool,
    pub groups_truncated: bool,
    /// How this session reached the directory + got its password (for the UI gap).
    pub conn: ConnInfo,
    /// This connection's single-domain config + resolved bind secret, retained so the
    /// per-session browse worker can be (re)spawned on focus (see `tui::browse`).
    pub cfg: Config,
    pub password: Option<String>,
    /// Per-connection write posture (read-only / write / dry-run).
    pub mode: ConnMode,
    /// Rail labels: the server (grouping) and the domain (leaf).
    #[allow(dead_code)] // surfaced by the connections rail (Increment C)
    pub server_label: String,
    #[allow(dead_code)]
    pub domain_label: String,
}

impl Session {
    /// Connect, bind, and load the initial user/group caches for one domain.
    pub fn connect(
        cfg: Config,
        password: Option<String>,
        pw_source: PwSource,
        mode: ConnMode,
        server_label: String,
        domain_label: String,
    ) -> anyhow::Result<Self> {
        let mut client = LdapClient::connect(&cfg, password.as_deref())?;
        let caps = client.caps();
        let tls = if cfg.server.use_ssl { "LDAPS" }
                  else if cfg.server.start_tls { "STARTTLS" } else { "LDAP" };
        let conn = ConnInfo { via: client.conn_via().clone(), tls, password: pw_source };
        let (users, users_truncated) = client.list_users()?;
        let (groups, groups_truncated) = client.list_groups()?;
        Ok(Self {
            client, caps, users, groups, users_truncated, groups_truncated, conn,
            cfg, password, mode, server_label, domain_label,
        })
    }

    /// `server · domain` — the combined label for titles/switchers.
    #[allow(dead_code)] // used by the rail/title (Increment C)
    pub fn label(&self) -> String {
        format!("{} · {}", self.server_label, self.domain_label)
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
