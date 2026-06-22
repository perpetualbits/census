use anyhow::Context;
use ldap3::{LdapConn, LdapConnSettings, Mod, Scope, SearchEntry};
use std::collections::{HashMap, HashSet};

use crate::config::{Config, TunnelConfig};
use super::tunnel::{self, Tunnel};

pub struct LdapClient {
    conn: LdapConn,
    pub base_dn: String,
    _tunnel: Option<Tunnel>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)] // full LDAP record; not every field is surfaced in the TUI yet
pub struct User {
    pub dn: String,
    pub uid: String,
    pub cn: String,
    pub uid_number: u32,
    pub gid_number: u32,
    pub home: String,
    pub shell: String,
    pub ssh_keys: Vec<String>,
    pub attrs: HashMap<String, Vec<String>>,
}

#[derive(Debug, Clone)]
#[allow(dead_code)] // gid_number kept for completeness; not shown in the TUI yet
pub struct Group {
    pub dn: String,
    pub name: String,
    pub gid_number: u32,
    pub members: Vec<String>,
}

impl LdapClient {
    pub fn connect(cfg: &Config, password: Option<&str>) -> anyhow::Result<Self> {
        let (host, port, tun) = resolve_endpoint(cfg)?;

        let url = if cfg.server.use_ssl {
            format!("ldaps://{host}:{port}")
        } else {
            format!("ldap://{host}:{port}")
        };

        let settings = LdapConnSettings::new()
            .set_no_tls_verify(!cfg.server.verify)
            .set_starttls(cfg.server.start_tls && !cfg.server.use_ssl);

        let mut conn = LdapConn::with_settings(settings, &url)
            .with_context(|| format!("Failed to connect to {url}"))?;

        match (cfg.server.bind_dn.as_deref(), password) {
            (Some(dn), Some(pw)) => {
                conn.simple_bind(dn, pw)
                    .context("Bind failed")?
                    .success()
                    .context("Bind rejected")?;
            }
            _ => {
                // anonymous
                conn.simple_bind("", "")
                    .context("Anonymous bind failed")?
                    .success()
                    .context("Anonymous bind rejected")?;
            }
        }

        Ok(Self {
            conn,
            base_dn: cfg.server.base_dn.clone(),
            _tunnel: tun,
        })
    }

    pub fn ping(&mut self) -> anyhow::Result<()> {
        // A BASE search on the root DSE is a lightweight connectivity check.
        let (_, res) = self.conn
            .search("", Scope::Base, "(objectClass=*)", vec!["vendorName", "vendorVersion"])
            .context("Root DSE search failed")?
            .success()
            .context("Root DSE search rejected")?;
        drop(res);
        Ok(())
    }

    // ---------- users -------------------------------------------------------

    pub fn list_users(&mut self) -> anyhow::Result<Vec<User>> {
        let base = format!("ou=users,{}", self.base_dn);
        let attrs = vec!["uid", "cn", "sn", "givenName", "uidNumber", "gidNumber",
                         "homeDirectory", "loginShell", "sshPublicKey"];
        let (rs, _) = self.conn
            .search(&base, Scope::OneLevel, "(objectClass=posixAccount)", attrs)
            .context("User search failed")?
            .success()
            .context("User search rejected")?;

        let mut users = Vec::new();
        for entry in rs {
            let e = SearchEntry::construct(entry);
            let uid = first(&e, "uid").unwrap_or_default();
            if uid.is_empty() { continue; }
            users.push(User {
                dn: e.dn.clone(),
                uid,
                cn: first(&e, "cn").unwrap_or_default(),
                uid_number: first(&e, "uidNumber").and_then(|s| s.parse().ok()).unwrap_or(0),
                gid_number: first(&e, "gidNumber").and_then(|s| s.parse().ok()).unwrap_or(0),
                home: first(&e, "homeDirectory").unwrap_or_default(),
                shell: first(&e, "loginShell").unwrap_or_default(),
                ssh_keys: e.attrs.get("sshPublicKey").cloned().unwrap_or_default(),
                attrs: e.attrs,
            });
        }
        users.sort_by(|a, b| a.uid.cmp(&b.uid));
        Ok(users)
    }

    #[allow(dead_code)] // detail-view lookup; wired up when per-user view lands
    pub fn get_user(&mut self, uid: &str) -> anyhow::Result<Option<User>> {
        let base = format!("ou=users,{}", self.base_dn);
        let filter = format!("(uid={uid})");
        let attrs = vec!["*", "+"];
        let (rs, _) = self.conn
            .search(&base, Scope::OneLevel, &filter, attrs)
            .context("User search failed")?
            .success()
            .context("User search rejected")?;

        Ok(rs.into_iter().next().map(|entry| {
            let e = SearchEntry::construct(entry);
            User {
                dn: e.dn.clone(),
                uid: first(&e, "uid").unwrap_or_default(),
                cn: first(&e, "cn").unwrap_or_default(),
                uid_number: first(&e, "uidNumber").and_then(|s| s.parse().ok()).unwrap_or(0),
                gid_number: first(&e, "gidNumber").and_then(|s| s.parse().ok()).unwrap_or(0),
                home: first(&e, "homeDirectory").unwrap_or_default(),
                shell: first(&e, "loginShell").unwrap_or_default(),
                ssh_keys: e.attrs.get("sshPublicKey").cloned().unwrap_or_default(),
                attrs: e.attrs,
            }
        }))
    }

    // ---------- groups ------------------------------------------------------

    pub fn list_groups(&mut self) -> anyhow::Result<Vec<Group>> {
        let base = format!("ou=groups,{}", self.base_dn);
        let attrs = vec!["cn", "gidNumber", "memberUid"];
        let (rs, _) = self.conn
            .search(&base, Scope::OneLevel, "(objectClass=posixGroup)", attrs)
            .context("Group search failed")?
            .success()
            .context("Group search rejected")?;

        let mut groups = Vec::new();
        for entry in rs {
            let e = SearchEntry::construct(entry);
            let name = first(&e, "cn").unwrap_or_default();
            if name.is_empty() { continue; }
            groups.push(Group {
                dn: e.dn.clone(),
                name,
                gid_number: first(&e, "gidNumber").and_then(|s| s.parse().ok()).unwrap_or(0),
                members: e.attrs.get("memberUid").cloned().unwrap_or_default(),
            });
        }
        groups.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(groups)
    }

    // ---------- group membership (write ops) --------------------------------

    pub fn group_add_member(&mut self, group_dn: &str, uid: &str) -> anyhow::Result<()> {
        self.conn
            .modify(group_dn, vec![Mod::Add("memberUid", HashSet::from([uid]))])
            .context("Modify add memberUid failed")?
            .success()
            .context("Modify add memberUid rejected")?;
        Ok(())
    }

    pub fn group_remove_member(&mut self, group_dn: &str, uid: &str) -> anyhow::Result<()> {
        self.conn
            .modify(group_dn, vec![Mod::Delete("memberUid", HashSet::from([uid]))])
            .context("Modify delete memberUid failed")?
            .success()
            .context("Modify delete memberUid rejected")?;
        Ok(())
    }

    #[allow(dead_code)] // attribute editing; wired up when the edit view lands
    pub fn set_attr(&mut self, dn: &str, attr: &str, value: &str) -> anyhow::Result<()> {
        self.conn
            .modify(dn, vec![Mod::Replace(attr, HashSet::from([value]))])
            .context("Modify replace failed")?
            .success()
            .context("Modify replace rejected")?;
        Ok(())
    }

    pub fn close(mut self) -> anyhow::Result<()> {
        self.conn.unbind().context("Unbind failed")?;
        Ok(())
    }
}

// ---------- helpers ---------------------------------------------------------

fn first(entry: &SearchEntry, attr: &str) -> Option<String> {
    entry.attrs.get(attr)?.first().cloned()
}

fn resolve_endpoint(cfg: &Config) -> anyhow::Result<(String, u16, Option<Tunnel>)> {
    let tc: &TunnelConfig = &cfg.tunnel;
    if !tc.enabled {
        return Ok((cfg.server.host.clone(), cfg.server.port, None));
    }

    let ssh_alias = tc.ssh_alias.as_deref()
        .or(Some(cfg.server.host.as_str()))
        .unwrap();
    let remote_host = tc.remote_host.as_deref().unwrap_or(&cfg.server.host);
    let remote_port = tc.remote_port.unwrap_or(cfg.server.port);

    let tun = tunnel::ensure(
        ssh_alias,
        remote_host,
        remote_port,
        std::time::Duration::from_secs(10),
    )?;

    let local_port = tun.local_port;
    Ok(("127.0.0.1".into(), local_port, Some(tun)))
}
