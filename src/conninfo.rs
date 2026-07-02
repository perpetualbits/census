//! How census reached the directory and obtained its bind password.
//!
//! Surfaced in the top-border "gap" (see [`crate::tui`]) so the operator can see
//! the connection path — direct vs SSH tunnel, and which secret source — at a
//! glance, without digging through config.

/// Which secret source supplied the bind password.
#[derive(Debug, Clone)]
pub enum PwSource {
    /// A `password_cmd` (e.g. `rbw get …`); the label is the command's program name.
    Cmd(String),
    /// The `CENSUS_BIND_PASSWORD` environment variable.
    Env,
    /// An interactive prompt at startup.
    Prompt,
    /// No bind DN configured — anonymous bind, no password.
    Anonymous,
}

impl PwSource {
    /// Short label for the border gap (`rbw`, `env`, `prompt`, `anon`).
    pub fn label(&self) -> &str {
        match self {
            PwSource::Cmd(l)     => l,
            PwSource::Env        => "env",
            PwSource::Prompt     => "prompt",
            PwSource::Anonymous  => "anon",
        }
    }
}

/// How the LDAP socket is reached.
#[derive(Debug, Clone)]
pub enum ConnVia {
    /// A direct TCP connection to `host`.
    Direct { host: String },
    /// An SSH local-forward tunnel via `alias`; `reused` is true when census
    /// attached to a forward it did not spawn.
    Tunnel { alias: String, reused: bool },
}

/// The full connection story shown in the border gap.
#[derive(Debug, Clone)]
pub struct ConnInfo {
    pub via: ConnVia,
    /// Transport label: `LDAPS`, `STARTTLS`, or `LDAP`.
    pub tls: &'static str,
    pub password: PwSource,
}

impl ConnInfo {
    /// One-line summary, e.g. `tunnel ldap.lofar.eu · LDAPS · rbw`.
    pub fn summary(&self) -> String {
        let via = match &self.via {
            ConnVia::Direct { host } => format!("direct {host}"),
            ConnVia::Tunnel { alias, reused } => {
                if *reused { format!("tunnel {alias} (reused)") } else { format!("tunnel {alias}") }
            }
        };
        format!("{via} · {} · {}", self.tls, self.password.label())
    }
}
