use serde::Deserialize;
use std::path::PathBuf;

#[derive(Deserialize, Debug, Clone)]
pub struct Config {
    pub server: ServerConfig,
    #[serde(default)]
    pub tunnel: TunnelConfig,
    #[serde(default)]
    pub display: DisplayConfig,
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

fn default_port() -> u16 { 636 }
fn default_true() -> bool { true }

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
