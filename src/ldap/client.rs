use anyhow::Context;
use ldap3::{LdapConn, LdapConnSettings, Mod, Scope, SearchEntry};
use std::collections::{HashMap, HashSet};

use crate::config::{Config, PwScheme, TunnelConfig};
use crate::schema::Schema;
use super::password::crypt_sha512;
use super::tunnel::{self, Tunnel};

pub struct LdapClient {
    conn: LdapConn,
    pub base_dn: String,
    schema: Schema,
    password_scheme: PwScheme,
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

/// Fields for creating a new user entry.
#[derive(Debug, Clone)]
pub struct NewUserSpec {
    pub uid: String,
    pub cn: String,
    pub sn: String,
    pub given_name: Option<String>,
    pub uid_number: u32,
    pub gid_number: u32,
    pub home: String,
    pub shell: String,
    pub password: Option<String>,
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
            password_scheme: cfg.server.password_scheme,
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

    // ---------- generic attribute modify -----------------------------------

    /// Replace an attribute's value set (empty `values` clears the attribute).
    pub fn modify_replace(&mut self, dn: &str, attr: &str, values: &[&str]) -> anyhow::Result<()> {
        let set: HashSet<&str> = values.iter().copied().collect();
        self.conn
            .modify(dn, vec![Mod::Replace(attr, set)])
            .with_context(|| format!("Modify replace {attr} failed"))?
            .success()
            .with_context(|| format!("Modify replace {attr} rejected"))?;
        Ok(())
    }

    /// Add values to an attribute (creating it if absent).
    pub fn modify_add(&mut self, dn: &str, attr: &str, values: &[&str]) -> anyhow::Result<()> {
        let set: HashSet<&str> = values.iter().copied().collect();
        self.conn
            .modify(dn, vec![Mod::Add(attr, set)])
            .with_context(|| format!("Modify add {attr} failed"))?
            .success()
            .with_context(|| format!("Modify add {attr} rejected"))?;
        Ok(())
    }

    /// Delete an attribute entirely (empty `values`) or specific values.
    #[allow(dead_code)] // available for callers that need value-level deletes
    pub fn modify_delete(&mut self, dn: &str, attr: &str, values: &[&str]) -> anyhow::Result<()> {
        let set: HashSet<&str> = values.iter().copied().collect();
        self.conn
            .modify(dn, vec![Mod::Delete(attr, set)])
            .with_context(|| format!("Modify delete {attr} failed"))?
            .success()
            .with_context(|| format!("Modify delete {attr} rejected"))?;
        Ok(())
    }

    /// Ensure `dn` has object class `oc`, adding it if absent.
    pub fn ensure_object_class(&mut self, dn: &str, oc: &str) -> anyhow::Result<()> {
        let (rs, _) = self.conn
            .search(dn, Scope::Base, "(objectClass=*)", vec!["objectClass"])
            .context("Read objectClass failed")?
            .success()
            .context("Read objectClass rejected")?;
        let has = rs.into_iter().next().is_some_and(|e| {
            SearchEntry::construct(e).attrs.get("objectClass")
                .is_some_and(|v| v.iter().any(|c| c.eq_ignore_ascii_case(oc)))
        });
        if !has {
            self.modify_add(dn, "objectClass", &[oc])?;
        }
        Ok(())
    }

    /// Replace the full set of SSH public keys (empty list clears them).
    /// Ensures the key object class is present before adding any keys.
    pub fn ssh_key_replace(&mut self, dn: &str, keys: &[String]) -> anyhow::Result<()> {
        let attr = self.schema.ssh_key;          // &'static str — no borrow of self
        let oc   = self.schema.ssh_object_class;
        if !keys.is_empty() {
            self.ensure_object_class(dn, oc)?;
        }
        let refs: Vec<&str> = keys.iter().map(String::as_str).collect();
        self.modify_replace(dn, attr, &refs)
    }

    /// Set a user's password using the configured scheme: the server-side RFC
    /// 3062 Password Modify exop (default) or client-side `{CRYPT}$6$`.
    pub fn set_password(&mut self, dn: &str, plaintext: &str) -> anyhow::Result<()> {
        match self.password_scheme {
            PwScheme::Exop => {
                use ldap3::exop::PasswordModify;
                let req = PasswordModify {
                    user_id: Some(dn),
                    old_pass: None,
                    new_pass: Some(plaintext),
                };
                self.conn
                    .extended(req)
                    .context("Password modify exop failed")?
                    .success()
                    .context("Password modify exop rejected")?;
                Ok(())
            }
            PwScheme::Crypt => {
                let hashed = crypt_sha512(plaintext)?;
                self.modify_replace(dn, "userPassword", &[&hashed])
            }
        }
    }

    // ---------- create / delete ---------------------------------------------

    /// Lowest free uidNumber above the current maximum (floored at 10000).
    pub fn next_uid_number(&mut self) -> anyhow::Result<u32> {
        let base   = self.schema.user_base(&self.base_dn);
        let filter = self.schema.user_filter;     // &'static str
        let attr   = self.schema.uid_number;
        let (rs, _) = self.conn
            .search(&base, Scope::OneLevel, filter, vec![attr])
            .context("uidNumber scan failed")?
            .success()
            .context("uidNumber scan rejected")?;
        let max = rs.into_iter()
            .filter_map(|e| {
                SearchEntry::construct(e).attrs.get(attr)
                    .and_then(|v| v.first())
                    .and_then(|s| s.parse::<u32>().ok())
            })
            .max()
            .unwrap_or(9999);
        Ok(max.max(9999) + 1)
    }

    /// Create a new user entry, then set its password if one was supplied.
    /// Returns the new entry's DN.
    pub fn add_user(&mut self, spec: &NewUserSpec) -> anyhow::Result<String> {
        let s = self.schema.clone();
        let dn = s.user_dn(&spec.uid, &self.base_dn);

        let one = |v: String| -> HashSet<String> { HashSet::from([v]) };
        let mut attrs: Vec<(String, HashSet<String>)> = vec![
            ("objectClass".into(), s.user_object_classes.iter().map(|c| c.to_string()).collect()),
            (s.uid.into(),        one(spec.uid.clone())),
            (s.cn.into(),         one(spec.cn.clone())),
            (s.sn.into(),         one(spec.sn.clone())),
            (s.uid_number.into(), one(spec.uid_number.to_string())),
            (s.gid_number.into(), one(spec.gid_number.to_string())),
            (s.home.into(),       one(spec.home.clone())),
            (s.shell.into(),      one(spec.shell.clone())),
        ];
        if let Some(g) = &spec.given_name {
            if !g.is_empty() {
                attrs.push((s.given_name.into(), one(g.clone())));
            }
        }

        self.conn
            .add(&dn, attrs)
            .context("Add user failed")?
            .success()
            .context("Add user rejected")?;

        if let Some(pw) = &spec.password {
            if !pw.is_empty() {
                self.set_password(&dn, pw)?;
            }
        }
        Ok(dn)
    }

    /// Delete an entry by DN.
    pub fn delete_entry(&mut self, dn: &str) -> anyhow::Result<()> {
        self.conn
            .delete(dn)
            .context("Delete failed")?
            .success()
            .context("Delete rejected")?;
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

    let ssh_alias = tc.ssh_alias.as_deref().unwrap_or(cfg.server.host.as_str());
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
