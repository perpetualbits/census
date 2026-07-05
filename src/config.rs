use serde::Deserialize;
use std::path::PathBuf;

#[derive(Deserialize, Debug, Clone)]
pub struct Config {
    pub server: ServerConfig,
    #[serde(default)]
    pub tunnel: TunnelConfig,
    #[serde(default)]
    pub display: DisplayConfig,
    #[serde(default)]
    pub browse: BrowseConfig,
    /// Extra domains (naming contexts / base-DNs) served by the same server. Empty
    /// means the file describes a single domain (`server.base_dn`) — the legacy shape.
    /// Each `[[domain]]` becomes its own connection, sharing the server's transport
    /// and (unless overridden) bind, so one slapd hosting `dc=jive`, `dc=nova`, … is
    /// one config file. See [`Config::expand`]. TOML key is `[[domain]]`.
    #[serde(default, rename = "domain")]
    pub domains: Vec<DomainConfig>,
}

/// One additional domain (base-DN) under [`Config::server`]. `mode`/`bind_dn`/
/// `password_cmd` override the server defaults for this domain only.
#[derive(Deserialize, Debug, Clone)]
pub struct DomainConfig {
    pub base_dn: String,
    #[serde(default)]
    pub mode: Option<ConnMode>,
    pub bind_dn: Option<String>,
    pub password_cmd: Option<String>,
}

/// Per-connection write posture: browse-only, live writes, or dry-run (writes are
/// previewed as LDIF, never sent). Parsed from `[[domain]] mode = "…"`; the final
/// mode of a session is resolved against the CLI flags in `main.rs`.
#[derive(Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ConnMode {
    #[default]
    ReadOnly,
    Write,
    DryRun,
}

#[allow(dead_code)] // consumed by per-connection mode gating (Increment B) + rail badges (C)
impl ConnMode {
    /// Short title/badge word.
    pub fn tag(self) -> &'static str {
        match self { ConnMode::ReadOnly => "read-only", ConnMode::Write => "write", ConnMode::DryRun => "dry-run" }
    }
    /// May this connection open write flows (write or dry-run)?
    pub fn can_write(self) -> bool { matches!(self, ConnMode::Write | ConnMode::DryRun) }
}

#[derive(Deserialize, Debug, Clone)]
pub struct ServerConfig {
    /// Optional human label for the server (rail grouping); defaults to the config
    /// file's stem.
    #[serde(default)]
    pub name: Option<String>,
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default)]
    pub use_ssl: bool,
    #[serde(default)]
    pub start_tls: bool,
    pub base_dn: String,
    pub bind_dn: Option<String>,
    /// Shell command whose stdout is the bind password (e.g. `rbw get "My LDAP"`).
    /// Preferred over CENSUS_BIND_PASSWORD env var; both beat interactive prompt.
    pub password_cmd: Option<String>,
    /// A `cn=config` admin (e.g. `cn=admin,cn=config`) and its password command, for
    /// managing an **OpenLDAP** server's schema — which lives under `cn=config`,
    /// unreachable via the data bind. Only needed to add schema on OpenLDAP (389-DS
    /// uses the Directory Manager). Leave unset otherwise.
    pub config_bind_dn: Option<String>,
    pub config_password_cmd: Option<String>,
    /// For creating/deleting a **domain** on OpenLDAP: a command that runs a shell
    /// SCRIPT (read from stdin) as root on the LDAP host — census pipes the required
    /// `mkdir`/`chown`/`rm` to it (a new backend needs its on-disk directory, which no
    /// LDAP client can create remotely). Examples: `ssh ldap.example.com sudo bash -s`,
    /// `podman exec -i census-ldap bash`. Without it, census prints the exact commands
    /// for you to run instead.
    pub provision_cmd: Option<String>,
    /// `user:group` slapd runs as, for chowning a new backend's directory
    /// (default `openldap:openldap`). OpenLDAP domain-create only.
    pub slapd_user: Option<String>,
    /// TLS SNI / certificate name override. Reserved for verified tunnel
    /// connections; not consulted by the rustls path yet.
    #[allow(dead_code)]
    pub sni: Option<String>,
    #[serde(default = "default_true")]
    pub verify: bool,
    /// How password changes are written: server-side RFC 3062 exop (default,
    /// honours the server's password policy) or client-side `{CRYPT}$6$`.
    #[serde(default)]
    pub password_scheme: PwScheme,
}

/// Strategy for writing a user's password.
#[derive(Deserialize, Debug, Clone, Copy, Default, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PwScheme {
    /// RFC 3062 Password Modify extended operation (server hashes per policy).
    #[default]
    Exop,
    /// Client-side SHA-512 crypt stored as `userPassword: {CRYPT}$6$…`.
    Crypt,
}

#[derive(Deserialize, Debug, Clone, Default)]
pub struct TunnelConfig {
    #[serde(default)]
    pub enabled: bool,
    pub ssh_alias: Option<String>,
    pub remote_host: Option<String>,
    pub remote_port: Option<u16>,
}

#[derive(Deserialize, Debug, Clone, Default)]
pub struct DisplayConfig {
    #[serde(default)]
    pub allow_writes: bool,
}

/// How the user browse list is ordered, pushed to the server as Server-Side Sort +
/// VLV. Defaults reproduce census's original behaviour (sort by `uid`, string
/// ordering). On a directory that ships a **precomputed sort-key attribute** with a
/// VLV browsing index — e.g. an integer `sortRank` giving surname-then-given order —
/// point census at it here so its SSS/VLV names the *same* attribute and ordering
/// rule the index was built with; that's what makes a million-entry ordered browse
/// hit the index (fast) instead of an in-memory re-sort. Where the attribute is
/// absent, leave the defaults and census sorts by `uid`.
#[derive(Deserialize, Debug, Clone)]
pub struct BrowseConfig {
    /// Attribute the browse list is sorted by (default `uid`). Must be unique per
    /// entry (it doubles as the paging key) and covered by the server's VLV index.
    #[serde(default = "default_sort_attr")]
    pub sort_attr: String,
    /// The orderingRule OID census names in its SSS control — it must match the rule
    /// the VLV index was built with, or the server won't use the index. Default is
    /// `caseIgnoreOrderingMatch` (2.5.13.3) for string keys; use `2.5.13.15`
    /// (integerOrderingMatch) for an integer key like `sortRank`.
    #[serde(default = "default_sort_ordering")]
    pub sort_ordering: String,
}

impl Default for BrowseConfig {
    fn default() -> Self {
        BrowseConfig { sort_attr: default_sort_attr(), sort_ordering: default_sort_ordering() }
    }
}

fn default_port() -> u16 { 636 }
fn default_true() -> bool { true }
fn default_sort_attr() -> String { "uid".to_string() }
fn default_sort_ordering() -> String { "2.5.13.3".to_string() }

/// One resolved connection: a single-domain [`Config`] ready for
/// `LdapClient::connect`/`Browse::spawn`, plus its rail labels and config-declared
/// mode (`None` = resolve from CLI flags / `allow_writes`). Produced by
/// [`Config::expand`].
#[derive(Debug, Clone)]
pub struct EffectiveConfig {
    pub cfg: Config,
    pub server_label: String,
    pub domain_label: String,
    pub mode: Option<ConnMode>,
}

impl Config {
    pub fn load(path: Option<&PathBuf>) -> anyhow::Result<Self> {
        let resolved = path.cloned().unwrap_or_else(config_path);
        if !resolved.exists() {
            anyhow::bail!(
                "Config file not found: {}\n\
                 Create it from docs/config.toml.example",
                resolved.display()
            );
        }
        let text = std::fs::read_to_string(&resolved)?;
        let cfg: Self = toml::from_str(&text)
            .map_err(|e| anyhow::anyhow!("Config parse error in {}: {e}", resolved.display()))?;
        Ok(cfg)
    }

    /// Fan a config file out into connections: `server.base_dn` is **always** the
    /// primary domain, and each `[[domain]]` adds another (sharing the server's
    /// transport, with optional per-domain base-DN / bind / secret / mode). A file
    /// with no `[[domain]]` yields exactly one connection — identical to the
    /// pre-multi-domain behaviour.
    pub fn expand(mut self, file_stem: &str) -> Vec<EffectiveConfig> {
        let server_label = self.server.name.clone().unwrap_or_else(|| file_stem.to_string());
        let domains = std::mem::take(&mut self.domains); // cfg is now single-domain
        // Primary domain = the server's own base_dn (mode from server defaults / CLI).
        let mut out = vec![EffectiveConfig {
            domain_label: domain_label(&self.server.base_dn),
            server_label: server_label.clone(),
            mode: None,
            cfg: self.clone(),
        }];
        // Plus each explicitly-listed extra domain.
        for d in domains {
            let mut cfg = self.clone();
            cfg.server.base_dn = d.base_dn.clone();
            if let Some(b) = &d.bind_dn { cfg.server.bind_dn = Some(b.clone()); }
            if let Some(p) = &d.password_cmd { cfg.server.password_cmd = Some(p.clone()); }
            out.push(EffectiveConfig {
                domain_label: domain_label(&d.base_dn),
                server_label: server_label.clone(),
                mode: d.mode,
                cfg,
            });
        }
        out
    }

    /// Resolve every connection census should open. An explicit `--config FILE` is
    /// used **exclusively** (back-compat: it selects that one file, expanded over its
    /// domains). Otherwise every `~/.config/census/conf.d/*.toml` is loaded in
    /// filename order; if that directory is absent or empty, census falls back to the
    /// single default `config.toml` — preserving today's behaviour exactly. Duplicate
    /// `(host, port, base_dn)` connections are skipped with a warning.
    pub fn load_all(explicit: Option<&PathBuf>) -> anyhow::Result<Vec<EffectiveConfig>> {
        if let Some(path) = explicit {
            let cfg = Config::load(Some(path))?;
            return Ok(dedup(cfg.expand(&file_stem(path))));
        }

        let dir = conf_d_path();
        let mut out = if dir.is_dir() { load_dir(&dir)? } else { Vec::new() };

        if out.is_empty() {
            // No conf.d — fall back to the single default config.toml (legacy path).
            let cfg = Config::load(None)?;
            out.extend(cfg.expand("config"));
        }
        Ok(dedup(out))
    }
}

/// Expand every `*.toml` in `dir` (filename order) into connections. Split out from
/// [`Config::load_all`] so it can be tested against a temp directory.
fn load_dir(dir: &std::path::Path) -> anyhow::Result<Vec<EffectiveConfig>> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .with_context_dir(dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "toml"))
        .collect();
    files.sort();
    let mut out = Vec::new();
    for path in files {
        let text = std::fs::read_to_string(&path)?;
        let cfg: Config = toml::from_str(&text)
            .map_err(|e| anyhow::anyhow!("Config parse error in {}: {e}", path.display()))?;
        out.extend(cfg.expand(&file_stem(&path)));
    }
    Ok(out)
}

/// Drop later connections that repeat an earlier `(host, port, base_dn)`, warning once.
fn dedup(list: Vec<EffectiveConfig>) -> Vec<EffectiveConfig> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::with_capacity(list.len());
    for ec in list {
        let key = (ec.cfg.server.host.clone(), ec.cfg.server.port, ec.cfg.server.base_dn.clone());
        if seen.insert(key) {
            out.push(ec);
        } else {
            eprintln!("Warning: skipping duplicate connection {}:{} {}",
                ec.cfg.server.host, ec.cfg.server.port, ec.cfg.server.base_dn);
        }
    }
    out
}

/// The short label for a domain: the value of the leftmost RDN of `base_dn`
/// (`dc=jive,dc=astron,dc=nl` → `jive`), or the whole DN if it can't be split.
pub fn domain_label(base_dn: &str) -> String {
    base_dn.split(',').next()
        .and_then(|rdn| rdn.split('=').nth(1))
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .unwrap_or(base_dn)
        .to_string()
}

/// A config file's stem (`astron-prod.toml` → `astron-prod`), the default server label.
fn file_stem(path: &std::path::Path) -> String {
    path.file_stem().and_then(|s| s.to_str()).unwrap_or("server").to_string()
}

/// `~/.config/census/conf.d`, respecting `XDG_CONFIG_HOME` (mirrors [`config_path`]).
fn conf_d_path() -> PathBuf {
    config_path().with_file_name("conf.d")
}

pub fn config_path() -> PathBuf {
    // Respect XDG_CONFIG_HOME if set, otherwise ~/.config
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".config")
        });
    base.join("census").join("config.toml")
}

/// Small extension to attach the directory path to a `read_dir` error.
trait DirContext<T> {
    fn with_context_dir(self, dir: &std::path::Path) -> anyhow::Result<T>;
}
impl<T> DirContext<T> for std::io::Result<T> {
    fn with_context_dir(self, dir: &std::path::Path) -> anyhow::Result<T> {
        self.map_err(|e| anyhow::anyhow!("reading {}: {e}", dir.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEGACY: &str = r#"
        [server]
        host = "ldap.astron.nl"
        base_dn = "dc=astron,dc=nl"
        bind_dn = "cn=admin,dc=astron,dc=nl"
    "#;

    const MULTI: &str = r#"
        [server]
        name = "astron-prod"
        host = "ldap.astron.nl"
        base_dn = "dc=astron,dc=nl"
        bind_dn = "cn=admin,dc=astron,dc=nl"
        password_cmd = "rbw get astron"

        [[domain]]
        base_dn = "dc=jive,dc=astron,dc=nl"
        mode = "read-only"

        [[domain]]
        base_dn = "dc=nova,dc=astron,dc=nl"
        mode = "write"
        bind_dn = "cn=dm,dc=nova,dc=astron,dc=nl"
        password_cmd = "rbw get nova"
    "#;

    fn parse(s: &str) -> Config { toml::from_str(s).unwrap() }

    #[test]
    fn legacy_single_server_expands_to_one_connection() {
        let ec = parse(LEGACY).expand("astron");
        assert_eq!(ec.len(), 1);
        assert_eq!(ec[0].cfg.server.base_dn, "dc=astron,dc=nl");
        assert_eq!(ec[0].server_label, "astron"); // no [server] name → file stem
        assert_eq!(ec[0].domain_label, "astron");
        assert_eq!(ec[0].mode, None); // resolved later from CLI/allow_writes
    }

    #[test]
    fn multi_domain_expands_primary_plus_extras_with_overrides() {
        let ec = parse(MULTI).expand("astron-prod");
        assert_eq!(ec.len(), 3); // primary (base_dn) + 2 [[domain]]
        assert!(ec.iter().all(|e| e.server_label == "astron-prod")); // [server] name wins

        // [0] = the server's own base_dn: the primary domain, mode from server default.
        let primary = &ec[0];
        assert_eq!(primary.cfg.server.base_dn, "dc=astron,dc=nl");
        assert_eq!(primary.domain_label, "astron");
        assert_eq!(primary.mode, None);

        let jive = &ec[1];
        assert_eq!(jive.cfg.server.base_dn, "dc=jive,dc=astron,dc=nl");
        assert_eq!(jive.domain_label, "jive");
        assert_eq!(jive.mode, Some(ConnMode::ReadOnly));
        // no per-domain bind override → inherits the server bind + secret
        assert_eq!(jive.cfg.server.bind_dn.as_deref(), Some("cn=admin,dc=astron,dc=nl"));
        assert_eq!(jive.cfg.server.password_cmd.as_deref(), Some("rbw get astron"));

        let nova = &ec[2];
        assert_eq!(nova.cfg.server.base_dn, "dc=nova,dc=astron,dc=nl");
        assert_eq!(nova.mode, Some(ConnMode::Write));
        assert_eq!(nova.cfg.server.bind_dn.as_deref(), Some("cn=dm,dc=nova,dc=astron,dc=nl"));
        assert_eq!(nova.cfg.server.password_cmd.as_deref(), Some("rbw get nova"));
        // the expanded config is single-domain (no nested domains left)
        assert!(nova.cfg.domains.is_empty());
    }

    #[test]
    fn domain_label_takes_leftmost_rdn() {
        assert_eq!(domain_label("dc=jive,dc=astron,dc=nl"), "jive");
        assert_eq!(domain_label("o=Nova,c=NL"), "Nova");
        assert_eq!(domain_label("weird"), "weird");
    }

    #[test]
    fn load_dir_merges_files_in_filename_order_and_dedups() {
        // A throwaway temp dir (no tempfile dep); unique by pid.
        let dir = std::env::temp_dir().join(format!("census-cfgtest-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // Written out of order; filenames drive the order (b after a).
        std::fs::write(dir.join("b-astron.toml"), MULTI).unwrap();
        std::fs::write(dir.join("a-lofar.toml"), r#"
            [server]
            host = "ldap.lofar.eu"
            base_dn = "dc=lofar,dc=eu"
        "#).unwrap();
        // A duplicate of lofar (same host/port/base_dn) → deduped by load_all's dedup().
        std::fs::write(dir.join("c-dup.toml"), r#"
            [server]
            host = "ldap.lofar.eu"
            base_dn = "dc=lofar,dc=eu"
        "#).unwrap();

        let all = dedup(load_dir(&dir).unwrap());
        std::fs::remove_dir_all(&dir).ok();

        // a-lofar (1) + b-astron (primary + 2 domains = 3) = 4; c-dup dropped.
        assert_eq!(all.len(), 4);
        assert_eq!(all[0].domain_label, "lofar");       // a- sorts first
        assert_eq!(all[1].domain_label, "astron");      // then b-astron: primary,
        assert_eq!(all[2].domain_label, "jive");        // then its extra domains
        assert_eq!(all[3].domain_label, "nova");
    }
}
