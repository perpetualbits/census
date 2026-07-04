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
}

#[derive(Deserialize, Debug, Clone)]
pub struct ServerConfig {
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
