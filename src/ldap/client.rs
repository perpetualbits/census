use anyhow::Context;
use ldap3::{LdapConn, LdapConnSettings, Mod, Scope, SearchEntry};
use std::collections::{HashMap, HashSet};

use crate::config::{Config, TunnelConfig};
use crate::schema::Schema;
use super::tunnel::{self, Tunnel};

pub struct LdapClient {
    conn: LdapConn,
    pub base_dn: String,
    schema: Schema,
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
            schema: Schema::rfc2307(),
            _tunnel: tun,
        })
    }

    /// The directory schema this client is bound to.
    #[allow(dead_code)] // consumed by the detail/edit views (P2/P4)
    pub fn schema(&self) -> &Schema { &self.schema }

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
        let s = &self.schema;
        let base = s.user_base(&self.base_dn);
        let attrs = vec![s.uid, s.cn, s.sn, s.given_name, s.uid_number, s.gid_number,
                         s.home, s.shell, s.ssh_key];
        let (rs, _) = self.conn
            .search(&base, Scope::OneLevel, s.user_filter, attrs)
            .context("User search failed")?
            .success()
            .context("User search rejected")?;

        let s = self.schema.clone();
        let mut users = Vec::new();
        for entry in rs {
            let e = SearchEntry::construct(entry);
            if let Some(user) = user_from_entry(e, &s) {
                users.push(user);
            }
        }
        users.sort_by(|a, b| a.uid.cmp(&b.uid));
        Ok(users)
    }

    /// Fetch a single user with all attributes (`*` + operational `+`).
    pub fn get_user(&mut self, uid: &str) -> anyhow::Result<Option<User>> {
        let s = &self.schema;
        let base = s.user_base(&self.base_dn);
        let filter = format!("({}={})", s.uid, ldap3::ldap_escape(uid));
        let attrs = vec!["*", "+"];
        let (rs, _) = self.conn
            .search(&base, Scope::OneLevel, &filter, attrs)
            .context("User search failed")?
            .success()
            .context("User search rejected")?;

        let s = self.schema.clone();
        Ok(rs.into_iter()
            .next()
            .and_then(|entry| user_from_entry(SearchEntry::construct(entry), &s)))
    }

    // ---------- groups ------------------------------------------------------

    pub fn list_groups(&mut self) -> anyhow::Result<Vec<Group>> {
        let s = &self.schema;
        let base = s.group_base(&self.base_dn);
        let attrs = vec![s.cn, s.gid_number, s.member];
        let (rs, _) = self.conn
            .search(&base, Scope::OneLevel, s.group_filter, attrs)
            .context("Group search failed")?
            .success()
            .context("Group search rejected")?;

        let s = &self.schema;
        let mut groups = Vec::new();
        for entry in rs {
            let e = SearchEntry::construct(entry);
            let name = first(&e, s.cn).unwrap_or_default();
            if name.is_empty() { continue; }
            groups.push(Group {
                dn: e.dn.clone(),
                name,
                gid_number: first(&e, s.gid_number).and_then(|s| s.parse().ok()).unwrap_or(0),
                members: e.attrs.get(s.member).cloned().unwrap_or_default(),
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

/// Build a [`User`] from a search entry, reading attribute names from `schema`.
/// Returns `None` when the entry has no RDN value (cannot identify the user).
fn user_from_entry(e: SearchEntry, schema: &Schema) -> Option<User> {
    let uid = first(&e, schema.uid).unwrap_or_default();
    if uid.is_empty() { return None; }
    Some(User {
        dn: e.dn.clone(),
        uid,
        cn: first(&e, schema.cn).unwrap_or_default(),
        uid_number: first(&e, schema.uid_number).and_then(|s| s.parse().ok()).unwrap_or(0),
        gid_number: first(&e, schema.gid_number).and_then(|s| s.parse().ok()).unwrap_or(0),
        home: first(&e, schema.home).unwrap_or_default(),
        shell: first(&e, schema.shell).unwrap_or_default(),
        ssh_keys: e.attrs.get(schema.ssh_key).cloned().unwrap_or_default(),
        attrs: e.attrs,
    })
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
