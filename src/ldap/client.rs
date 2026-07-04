use anyhow::Context;
use ldap3::{LdapConn, LdapConnSettings, Mod, ResultEntry, Scope, SearchEntry, SearchOptions};
use serde::Serialize;
use std::collections::{HashMap, HashSet};

/// Cap on how many entries a browse/list search pulls back, so a pathological
/// container (a huge `ou`, a DNS zone) degrades to "first N shown" instead of
/// hanging the UI or exhausting memory. Far above any real census user/group list;
/// census is not (yet) a virtualized browser — see docs/mullion-asks-round4.md.
pub(crate) const LIST_CAP: i32 = 5_000;

use crate::config::{Config, PwScheme, TunnelConfig};
use crate::conninfo::ConnVia;
use crate::schema::Schema;
use super::controls;
use super::password::crypt_sha512;
use super::tunnel::{self, Tunnel};

/// Which optional server controls the directory advertises (from the rootDSE), so
/// census can pick a paging strategy: SSS unlocks keyset browsing of huge lists.
#[derive(Debug, Clone, Copy, Default)]
#[allow(dead_code)] // vlv/paged drive Phase B (exact scrollbar, sequential fallback)
pub struct Caps {
    /// Server-Side Sort (RFC 2891) — required for keyset paging.
    pub sss: bool,
    /// Virtual List View (RFC 2891) — exact scrollbar position (Phase B).
    pub vlv: bool,
    /// Simple Paged Results (RFC 2696).
    pub paged: bool,
}

pub struct LdapClient {
    conn: LdapConn,
    pub base_dn: String,
    schema: Schema,
    password_scheme: PwScheme,
    caps: Caps,
    /// Attribute + orderingRule OID the browse list is sorted by (from `[browse]`
    /// config; default `uid` / caseIgnoreOrderingMatch). See [`BrowseConfig`].
    browse_sort_attr: String,
    browse_sort_ordering: String,
    _tunnel: Option<Tunnel>,
    conn_via: ConnVia,
}

#[derive(Debug, Clone, Serialize)]
#[allow(dead_code)] // full LDAP record; not every field is surfaced in the TUI yet
pub struct User {
    pub dn: String,
    pub uid: String,
    pub cn: String,
    /// `sn` (surname / last name); empty when absent. Also present in `attrs`.
    pub sn: String,
    /// `givenName` (first name); empty when absent. Also present in `attrs`.
    pub given_name: String,
    pub uid_number: u32,
    pub gid_number: u32,
    pub home: String,
    pub shell: String,
    pub ssh_keys: Vec<String>,
    /// Raw `jpegPhoto` bytes, when the entry carries one (binary attribute).
    /// Skipped in JSON output (binary); `attrs` carries the textual record.
    #[serde(skip)]
    pub photo: Option<Vec<u8>>,
    /// The browse **paging key**: the value of the configured browse sort attribute
    /// (default `uid`, e.g. `sortRank`). Set only on the VLV browse path
    /// ([`page_users_vlv`](LdapClient::page_users_vlv)); empty elsewhere, where the
    /// source falls back to keying by `uid`. Internal — not part of the JSON record.
    #[serde(skip)]
    pub sort_key: String,
    pub attrs: HashMap<String, Vec<String>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Group {
    pub dn: String,
    /// Authoritative name: the RDN value from the DN (e.g. `cn=lofar` → `lofar`).
    /// This is stable even when the entry carries a multi-valued `cn`.
    pub name: String,
    /// Other `cn` values on the entry (aliases), i.e. every `cn` that isn't `name`.
    pub aliases: Vec<String>,
    /// Parsed `gidNumber`, or `None` when the entry carries none (or an unparseable one).
    pub gid_number: Option<u32>,
    pub members: Vec<String>,
    /// Some `cn` of this group (name or alias) also appears on another group.
    pub dup_name: bool,
    /// Another group in the directory shares this `gidNumber`.
    pub dup_gid: bool,
    /// Full attribute record (populated by [`LdapClient::list_groups`]), for the detail pane.
    pub attrs: HashMap<String, Vec<String>>,
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

/// One child entry in the directory tree, for the DIT browser.
#[derive(Debug, Clone, Serialize)]
pub struct DitNode {
    pub dn: String,
    /// The RDN value, shown as the tree label (e.g. `ou=users` → `users`).
    pub rdn: String,
}

impl LdapClient {
    pub fn connect(cfg: &Config, password: Option<&str>) -> anyhow::Result<Self> {
        let (host, port, tun, conn_via) = resolve_endpoint(cfg)?;

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

        let caps = detect_caps(&mut conn);

        Ok(Self {
            conn,
            base_dn: cfg.server.base_dn.clone(),
            schema: Schema::rfc2307(),
            password_scheme: cfg.server.password_scheme,
            caps,
            browse_sort_attr: cfg.browse.sort_attr.clone(),
            browse_sort_ordering: cfg.browse.sort_ordering.clone(),
            _tunnel: tun,
            conn_via,
        })
    }

    /// Which paging-related controls the server supports.
    pub fn caps(&self) -> Caps { self.caps }

    /// One **VLV page**: the window of `before`+`after` entries around the sort
    /// position of `target` (or the list start when `target` is `None`), sorted by
    /// `sortattr` (Server-Side Sort). Unlike a `(sortattr>=key)` range filter, VLV
    /// positions via the *sort* — so it works even when the attribute has no schema
    /// ORDERING rule (`uid` on OpenLDAP). Returns the entries (ascending) and the VLV
    /// response `(targetPosition, contentCount)` for an exact scrollbar. Needs SSS+VLV.
    #[allow(clippy::too_many_arguments)]
    pub fn vlv_page(
        &mut self,
        base: &str,
        filter: &str,
        sortattr: &str,
        ordering: &str,
        attrs: &[&str],
        target: Option<&str>,
        before: i32,
        after: i32,
    ) -> anyhow::Result<(Vec<SearchEntry>, u64, u64)> {
        let sss = controls::sort_control(sortattr, ordering, false);
        let vlv = match target {
            Some(t) => controls::vlv_by_value(before, after, t),
            None => controls::vlv_by_offset(before, after, 1, 0),
        };
        let res = self.conn
            .with_controls(vec![sss, vlv])
            .search(base, Scope::OneLevel, filter, attrs.to_vec())
            .context("VLV search failed")?;
        if !matches!(res.1.rc, 0 | 4) {
            anyhow::bail!("VLV search rejected (rc {})", res.1.rc);
        }
        let (pos, count) = res.1.ctrls.iter()
            .find(|c| c.1.ctype == controls::VLV_RESPONSE_OID)
            .and_then(|c| c.1.val.as_deref())
            .and_then(controls::parse_vlv_response)
            .unwrap_or((0, 0));
        let entries = res.0.into_iter().map(SearchEntry::construct).collect();
        Ok((entries, pos, count))
    }

    /// A [`vlv_page`](Self::vlv_page) of [`User`]s sorted by the configured **browse
    /// sort attribute** (default `uid`; e.g. a precomputed `sortRank`) — the primitive
    /// [`UserSource`](super::source::UserSource) drives on SSS+VLV servers. Each user's
    /// [`sort_key`](User::sort_key) is set from that attribute so the source can key its
    /// window by it. Returns `(users, targetPosition, contentCount)`.
    pub fn page_users_vlv(
        &mut self,
        target: Option<&str>,
        before: i32,
        after: i32,
    ) -> anyhow::Result<(Vec<User>, u64, u64)> {
        let base = self.schema.user_base(&self.base_dn);
        let filter = self.schema.user_filter;
        let sort_attr = self.browse_sort_attr.clone();
        let ordering = self.browse_sort_ordering.clone();
        let mut attrs = {
            let s = &self.schema;
            vec![s.uid, s.cn, s.sn, s.given_name, s.uid_number, s.gid_number, s.home, s.shell, s.ssh_key]
        };
        // Fetch the sort attribute too (unless it's already among the user attrs), so
        // each row carries its own paging key.
        if !attrs.contains(&sort_attr.as_str()) {
            attrs.push(sort_attr.as_str());
        }
        let (entries, pos, count) =
            self.vlv_page(&base, filter, &sort_attr, &ordering, &attrs, target, before, after)?;
        let schema = self.schema.clone();
        let users = entries.into_iter()
            .filter_map(|e| user_from_entry(e, &schema))
            .map(|mut u| { set_sort_key(&mut u, &sort_attr); u })
            .collect();
        Ok((users, pos, count))
    }

    /// Server-side user search: a bounded substring filter over uid/cn/sn/givenName
    /// (plus exact uid/gidNumber when the query is numeric). Scales to huge dirs —
    /// the server does the filtering; census only ranks the small result set.
    pub fn search_users(&mut self, query: &str, limit: i32) -> anyhow::Result<Vec<User>> {
        let base = self.schema.user_base(&self.base_dn);
        let s = self.schema.clone();
        let q = ldap3::ldap_escape(query);
        let mut ors = format!("({}=*{q}*)({}=*{q}*)({}=*{q}*)({}=*{q}*)", s.uid, s.cn, s.sn, s.given_name);
        if !query.is_empty() && query.bytes().all(|b| b.is_ascii_digit()) {
            ors.push_str(&format!("({}={q})({}={q})", s.uid_number, s.gid_number));
        }
        let filter = format!("(&{}(|{ors}))", s.user_filter);
        let attrs = vec![s.uid, s.cn, s.sn, s.given_name, s.uid_number, s.gid_number, s.home, s.shell, s.ssh_key];
        let res = self.conn
            .with_search_options(SearchOptions::new().sizelimit(limit))
            .search(&base, Scope::OneLevel, &filter, attrs)
            .context("user search failed")?;
        if !matches!(res.1.rc, 0 | 4) {
            anyhow::bail!("user search rejected (rc {})", res.1.rc);
        }
        Ok(res.0.into_iter().filter_map(|e| user_from_entry(SearchEntry::construct(e), &s)).collect())
    }

    /// Server-side group search: a bounded substring filter over cn (plus exact
    /// gidNumber when numeric). Returns parsed groups (no duplicate flagging).
    pub fn search_groups(&mut self, query: &str, limit: i32) -> anyhow::Result<Vec<Group>> {
        let base = self.schema.group_base(&self.base_dn);
        let s = self.schema.clone();
        let q = ldap3::ldap_escape(query);
        let mut ors = format!("({}=*{q}*)", s.cn);
        if !query.is_empty() && query.bytes().all(|b| b.is_ascii_digit()) {
            ors.push_str(&format!("({}={q})", s.gid_number));
        }
        let filter = format!("(&{}(|{ors}))", s.group_filter);
        let res = self.conn
            .with_search_options(SearchOptions::new().sizelimit(limit))
            .search(&base, Scope::OneLevel, &filter, vec!["*"])
            .context("group search failed")?;
        if !matches!(res.1.rc, 0 | 4) {
            anyhow::bail!("group search rejected (rc {})", res.1.rc);
        }
        let groups = res.0.into_iter().map(|e| group_from_entry(SearchEntry::construct(e), &s)).collect();
        Ok(groups)
    }

    /// How this client reached the directory (direct vs SSH tunnel), for the UI.
    pub fn conn_via(&self) -> &ConnVia { &self.conn_via }

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

    /// Run a size-capped browse search ([`LIST_CAP`]). Returns the entries and whether
    /// the server hit the cap: `sizeLimitExceeded` (rc 4) means the partial results are
    /// valid and more entries exist; rc 0 is a complete result; any other non-zero rc is
    /// a real error.
    fn capped_search(&mut self, base: &str, scope: Scope, filter: &str, attrs: Vec<&str>)
        -> anyhow::Result<(Vec<ResultEntry>, bool)>
    {
        let res = self.conn
            .with_search_options(SearchOptions::new().sizelimit(LIST_CAP))
            .search(base, scope, filter, attrs)
            .context("search failed")?;
        match res.1.rc {
            0 => Ok((res.0, false)),
            4 => Ok((res.0, true)), // sizeLimitExceeded: partial results are valid
            rc => anyhow::bail!("search rejected (rc {rc})"),
        }
    }

    // ---------- users -------------------------------------------------------

    pub fn list_users(&mut self) -> anyhow::Result<(Vec<User>, bool)> {
        let base = self.schema.user_base(&self.base_dn);
        let filter = self.schema.user_filter;
        let attrs = {
            let s = &self.schema;
            vec![s.uid, s.cn, s.sn, s.given_name, s.uid_number, s.gid_number,
                 s.home, s.shell, s.ssh_key]
        };
        let (rs, truncated) = self.capped_search(&base, Scope::OneLevel, filter, attrs)?;

        let s = self.schema.clone();
        let mut users = Vec::new();
        for entry in rs {
            let e = SearchEntry::construct(entry);
            if let Some(user) = user_from_entry(e, &s) {
                users.push(user);
            }
        }
        users.sort_by(|a, b| a.uid.cmp(&b.uid));
        Ok((users, truncated))
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

    pub fn list_groups(&mut self) -> anyhow::Result<(Vec<Group>, bool)> {
        let base = self.schema.group_base(&self.base_dn);
        let filter = self.schema.group_filter;
        // Fetch ONLY the list-visible attributes (name + gidNumber) — never `memberUid`.
        // On a large directory a single group can hold millions of members, so pulling
        // `*` here loaded gigabytes and stalled startup. Members are read on demand for
        // the one group whose membership is being edited (see `group_members`).
        let s = &self.schema;
        let attrs = vec![s.cn, s.gid_number];
        let (rs, truncated) = self.capped_search(&base, Scope::OneLevel, filter, attrs)?;

        let s = self.schema.clone();
        let mut groups: Vec<Group> = rs.into_iter()
            .map(|entry| group_from_entry(SearchEntry::construct(entry), &s))
            .collect();
        flag_duplicates(&mut groups);
        groups.sort_by(|a, b| a.name.cmp(&b.name));
        Ok((groups, truncated))
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

    /// Lowest free gidNumber above the current maximum (floored at 10000).
    pub fn next_gid_number(&mut self) -> anyhow::Result<u32> {
        let base   = self.schema.group_base(&self.base_dn);
        let filter = self.schema.group_filter;
        let attr   = self.schema.gid_number;
        let (rs, _) = self.conn
            .search(&base, Scope::OneLevel, filter, vec![attr])
            .context("gidNumber scan failed")?
            .success()
            .context("gidNumber scan rejected")?;
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

    /// Create a posixGroup with the given name, gidNumber, and initial members.
    /// Returns the new entry's DN.
    pub fn add_group(&mut self, name: &str, gid_number: u32, members: &[String]) -> anyhow::Result<String> {
        let s = self.schema.clone();
        let dn = format!("{}={},{},{}", s.cn, name, s.group_ou, self.base_dn);
        let one = |v: String| -> HashSet<String> { HashSet::from([v]) };
        let mut attrs: Vec<(String, HashSet<String>)> = vec![
            ("objectClass".into(), HashSet::from(["top".to_string(), "posixGroup".to_string()])),
            (s.cn.into(),         one(name.to_string())),
            (s.gid_number.into(), one(gid_number.to_string())),
        ];
        if !members.is_empty() {
            attrs.push((s.member.into(), members.iter().cloned().collect()));
        }
        self.conn
            .add(&dn, attrs)
            .context("Add group failed")?
            .success()
            .context("Add group rejected")?;
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

    /// Rename a group entry: change its `cn` RDN to `new_cn` via LDAP modrdn
    /// (deleting the old RDN value), keeping it under the same parent. Returns the
    /// new DN. `memberUid`-based membership and `gidNumber` references are unaffected.
    pub fn rename_entry(&mut self, dn: &str, new_cn: &str) -> anyhow::Result<String> {
        let new_rdn = format!("cn={new_cn}");
        self.conn
            .modifydn(dn, &new_rdn, true, None)
            .context("Rename (modrdn) failed")?
            .success()
            .context("Rename (modrdn) rejected")?;
        let tail = dn.split_once(',').map(|(_, rest)| rest).unwrap_or("");
        Ok(if tail.is_empty() { new_rdn } else { format!("{new_rdn},{tail}") })
    }

    /// Read every user attribute of an entry as raw bytes (`*`, no operational
    /// attributes), so it can be re-created verbatim later. Text and binary
    /// attributes are merged into one `name → values` list. Used to capture the
    /// pre-state of a delete for rollback.
    pub fn read_entry_raw(&mut self, dn: &str) -> anyhow::Result<Vec<(String, Vec<Vec<u8>>)>> {
        let (rs, _) = self.conn
            .search(dn, Scope::Base, "(objectClass=*)", vec!["*"])
            .context("Read entry failed")?
            .success()
            .context("Read entry rejected")?;
        let Some(entry) = rs.into_iter().next() else {
            anyhow::bail!("Entry {dn} not found");
        };
        let e = SearchEntry::construct(entry);
        let mut attrs: Vec<(String, Vec<Vec<u8>>)> = Vec::new();
        for (name, vals) in e.attrs {
            attrs.push((name, vals.into_iter().map(String::into_bytes).collect()));
        }
        for (name, vals) in e.bin_attrs {
            attrs.push((name, vals));
        }
        Ok(attrs)
    }

    /// Re-create an entry from raw captured attributes (the inverse of a delete).
    pub fn add_raw(&mut self, dn: &str, attrs: &[(String, Vec<Vec<u8>>)]) -> anyhow::Result<()> {
        let add_attrs: Vec<(&[u8], HashSet<&[u8]>)> = attrs.iter()
            .map(|(name, vals)| {
                let set: HashSet<&[u8]> = vals.iter().map(Vec::as_slice).collect();
                (name.as_bytes(), set)
            })
            .collect();
        self.conn
            .add(dn, add_attrs)
            .context("Restore add failed")?
            .success()
            .context("Restore add rejected")?;
        Ok(())
    }

    // ---------- DIT browser -------------------------------------------------

    /// One level of children directly under `base` (for the tree browser). Sorted
    /// by RDN. An empty result means `base` is a leaf.
    pub fn list_children(&mut self, base: &str) -> anyhow::Result<(Vec<DitNode>, bool)> {
        let (rs, truncated) =
            self.capped_search(base, Scope::OneLevel, "(objectClass=*)", vec!["1.1"])?;
        let mut out = Vec::new();
        for entry in rs {
            let e = SearchEntry::construct(entry);
            let rdn = rdn_value(&e.dn).unwrap_or_else(|| e.dn.clone());
            out.push(DitNode { dn: e.dn, rdn });
        }
        out.sort_by_key(|n| n.rdn.to_lowercase());
        Ok((out, truncated))
    }

    /// An entry's attributes as displayable strings (binary values shown as a
    /// `(binary, N bytes)` placeholder), for the DIT detail pane.
    pub fn read_entry_display(&mut self, dn: &str) -> anyhow::Result<Vec<(String, Vec<String>)>> {
        let raw = self.read_entry_raw(dn)?;
        let mut out: Vec<(String, Vec<String>)> = raw.into_iter()
            .map(|(name, vals)| {
                let shown = vals.into_iter().map(|v| match String::from_utf8(v) {
                    Ok(s) => s,
                    Err(e) => format!("(binary, {} bytes)", e.into_bytes().len()),
                }).collect();
                (name, shown)
            })
            .collect();
        out.sort_by_key(|(k, _)| k.to_lowercase());
        Ok(out)
    }

    pub fn close(mut self) -> anyhow::Result<()> {
        self.conn.unbind().context("Unbind failed")?;
        Ok(())
    }
}

// ---------- helpers ---------------------------------------------------------

/// Read the rootDSE `supportedControl` and note the paging controls census cares
/// about. Best-effort: any failure yields all-false (census falls back to bounded
/// loads rather than erroring).
fn detect_caps(conn: &mut LdapConn) -> Caps {
    let oids: HashSet<String> = conn
        .search("", Scope::Base, "(objectClass=*)", vec!["supportedControl"])
        .ok()
        .and_then(|r| r.success().ok())
        .and_then(|(rs, _)| rs.into_iter().next())
        .map(|e| SearchEntry::construct(e).attrs.get("supportedControl").cloned().unwrap_or_default())
        .unwrap_or_default()
        .into_iter()
        .collect();
    Caps {
        sss: oids.contains(controls::SSS_OID),
        vlv: oids.contains(controls::VLV_OID),
        paged: oids.contains(controls::PAGED_OID),
    }
}

fn first(entry: &SearchEntry, attr: &str) -> Option<String> {
    entry.attrs.get(attr)?.first().cloned()
}

/// Mark groups whose `cn` (any value — name or alias, case-insensitive) or
/// `gidNumber` collides with another group in the set. This catches the subtle
/// case where one group's *alias* `cn` shadows another group's primary name (e.g.
/// `cn=lofar` also carrying `cn: cobalt`, colliding with the separate `cn=cobalt`).
/// The synthetic "(no cn)" placeholder is excluded from name collisions.
fn flag_duplicates(groups: &mut [Group]) {
    use std::collections::{HashMap, HashSet};

    // Every distinct cn value a group carries, lowercased, minus the placeholder.
    let cns_of = |g: &Group| -> HashSet<String> {
        std::iter::once(&g.name)
            .chain(g.aliases.iter())
            .filter(|c| *c != "(no cn)")
            .map(|c| c.to_lowercase())
            .collect()
    };

    let mut name_counts: HashMap<String, u32> = HashMap::new();
    let mut gid_counts:  HashMap<u32, u32>    = HashMap::new();
    for g in groups.iter() {
        for cn in cns_of(g) {
            *name_counts.entry(cn).or_default() += 1;
        }
        if let Some(gid) = g.gid_number {
            *gid_counts.entry(gid).or_default() += 1;
        }
    }
    for g in groups.iter_mut() {
        g.dup_name = cns_of(g).iter().any(|cn| name_counts.get(cn).copied().unwrap_or(0) > 1);
        g.dup_gid  = g.gid_number.is_some_and(|gid| gid_counts.get(&gid).copied().unwrap_or(0) > 1);
    }
}

/// The RDN attribute value of a DN, e.g. `cn=lofar,ou=groups,dc=…` → `lofar`.
/// A minimal parser: first comma-delimited component, value after the first `=`.
pub fn rdn_value(dn: &str) -> Option<String> {
    let first = dn.split(',').next()?;
    let (_attr, val) = first.split_once('=')?;
    Some(val.trim().to_string())
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
        sn: first(&e, schema.sn).unwrap_or_default(),
        given_name: first(&e, schema.given_name).unwrap_or_default(),
        uid_number: first(&e, schema.uid_number).and_then(|s| s.parse().ok()).unwrap_or(0),
        gid_number: first(&e, schema.gid_number).and_then(|s| s.parse().ok()).unwrap_or(0),
        home: first(&e, schema.home).unwrap_or_default(),
        shell: first(&e, schema.shell).unwrap_or_default(),
        ssh_keys: e.attrs.get(schema.ssh_key).cloned().unwrap_or_default(),
        // jpegPhoto is binary, so ldap3 surfaces it under `bin_attrs`, not `attrs`.
        photo: e.bin_attrs.get(schema.photo).and_then(|v| v.first()).cloned(),
        sort_key: String::new(), // set by the VLV browse path via set_sort_key
        attrs: e.attrs,
    })
}

/// Set a user's [`sort_key`](User::sort_key) from the browse sort attribute's value
/// (falling back to `uid` if the entry didn't carry it), so the browse source can key
/// its window by whatever attribute the server sorted on.
fn set_sort_key(u: &mut User, sort_attr: &str) {
    u.sort_key = u.attrs.get(sort_attr)
        .and_then(|v| v.first())
        .cloned()
        .unwrap_or_else(|| u.uid.clone());
}

/// Build a [`Group`] from a search entry (duplicate flags left unset — those are a
/// whole-set computation done separately by [`flag_duplicates`]). The authoritative
/// name is the RDN value, not `cn[0]`: an entry can carry a multi-valued `cn` in any
/// order, and the RDN is what identifies it. A nameless group is never dropped.
fn group_from_entry(e: SearchEntry, s: &Schema) -> Group {
    let all_cns = e.attrs.get(s.cn).cloned().unwrap_or_default();
    let name = rdn_value(&e.dn)
        .filter(|v| !v.is_empty())
        .or_else(|| all_cns.first().cloned())
        .unwrap_or_else(|| "(no cn)".to_string());
    let aliases: Vec<String> = all_cns.iter()
        .filter(|c| !c.eq_ignore_ascii_case(&name))
        .cloned()
        .collect();
    let gid_number = first(&e, s.gid_number).and_then(|v| v.parse::<u32>().ok());
    let members = e.attrs.get(s.member).cloned().unwrap_or_default();
    Group { dn: e.dn.clone(), name, aliases, gid_number, members, dup_name: false, dup_gid: false, attrs: e.attrs }
}

fn resolve_endpoint(cfg: &Config) -> anyhow::Result<(String, u16, Option<Tunnel>, ConnVia)> {
    let tc: &TunnelConfig = &cfg.tunnel;
    if !tc.enabled {
        let via = ConnVia::Direct { host: cfg.server.host.clone() };
        return Ok((cfg.server.host.clone(), cfg.server.port, None, via));
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

    let via = ConnVia::Tunnel { alias: ssh_alias.to_string(), reused: tun.is_reused() };
    let local_port = tun.local_port;
    Ok(("127.0.0.1".into(), local_port, Some(tun), via))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn group(dn: &str, name: &str, aliases: &[&str], gid: Option<u32>) -> Group {
        Group {
            dn: dn.into(),
            name: name.into(),
            aliases: aliases.iter().map(|s| s.to_string()).collect(),
            gid_number: gid,
            members: vec![],
            dup_name: false,
            dup_gid: false,
            attrs: HashMap::new(),
        }
    }

    #[test]
    fn rdn_value_extracts_the_rdn() {
        assert_eq!(rdn_value("cn=lofar,ou=groups,dc=lofar,dc=eu").as_deref(), Some("lofar"));
        assert_eq!(rdn_value("cn=cblt,ou=groups,dc=lofar,dc=eu").as_deref(), Some("cblt"));
        assert_eq!(rdn_value("bogus").as_deref(), None);
    }

    /// The live LOFAR collision: `cn=lofar` also carries `cn: cobalt` (gid 9000),
    /// a separate `cn=cobalt` (gid 10000), and `cn=cblt` (gid 10000).
    #[test]
    fn flags_alias_and_gid_collisions() {
        let mut groups = vec![
            group("cn=lofar,ou=groups,dc=lofar,dc=eu",  "lofar",  &["cobalt"], Some(9000)),
            group("cn=cobalt,ou=groups,dc=lofar,dc=eu", "cobalt", &[],         Some(10000)),
            group("cn=cblt,ou=groups,dc=lofar,dc=eu",   "cblt",   &[],         Some(10000)),
            group("cn=hpc,ou=groups,dc=lofar,dc=eu",    "hpc",    &[],         Some(10006)),
        ];
        flag_duplicates(&mut groups);
        let by = |n: &str| groups.iter().find(|g| g.name == n).unwrap();

        // "cobalt" appears as lofar's alias AND as a primary name → both flagged.
        assert!(by("lofar").dup_name,  "lofar carries the shared alias 'cobalt'");
        assert!(by("cobalt").dup_name, "cobalt's name is shadowed by lofar's alias");
        // gid 10000 is shared by cobalt and cblt.
        assert!(by("cobalt").dup_gid && by("cblt").dup_gid);
        // lofar's gid 9000 and hpc are unique.
        assert!(!by("lofar").dup_gid && !by("hpc").dup_gid && !by("hpc").dup_name);
        // cblt's name is unique (only its gid collides).
        assert!(!by("cblt").dup_name);
    }
}
