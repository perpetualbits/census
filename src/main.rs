mod cli;
mod config;
mod conninfo;
mod ldap;
mod schema;
mod session;
mod tui;

use std::collections::HashMap;
use std::path::PathBuf;

use clap::Parser;

use config::{ConnMode, Config};
use session::Session;

#[derive(Parser, Debug)]
#[command(name = "census", about = "LDAP user and group administration — TUI and scripting CLI")]
struct Args {
    /// Path to config file (default: ~/.config/census/config.toml)
    #[arg(long, value_name = "FILE", global = true)]
    config: Option<PathBuf>,

    /// Allow write operations (add/remove group members, edit attributes)
    #[arg(long, global = true)]
    write: bool,

    /// Walk through every write flow but send nothing — each write reports the
    /// LDAP operation it *would* perform (and its LDIF). Implies write-capable.
    #[arg(long, global = true)]
    dry_run: bool,

    /// Connect and exit; prints user and group counts. (Alias for `census ping`.)
    #[arg(long)]
    ping: bool,

    /// A scripting subcommand. Omit it to launch the interactive TUI.
    #[command(subcommand)]
    command: Option<cli::Command>,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    // A subcommand runs headless and exits; the legacy `--ping` flag maps to it.
    // The scripting CLI stays single-connection: load one config (the `--config` file
    // or the default) exactly as before.
    if args.command.is_some() || args.ping {
        let cfg = Config::load(args.config.as_ref())?;
        let allow_writes = args.write || cfg.display.allow_writes;
        let (password, _) = get_password(&cfg);
        let cmd = args.command.unwrap_or(cli::Command::Ping);
        return cli::dispatch(cmd, &cfg, password.as_deref(), allow_writes, args.dry_run);
    }

    // TUI: connect to every configured domain (conf.d/*.toml or a single --config /
    // default file, each expanded over its [[domain]] list).
    let conns = Config::load_all(args.config.as_ref())?;
    if conns.is_empty() {
        anyhow::bail!("No LDAP connections configured");
    }

    // One password resolution per distinct (bind_dn, password_cmd): a server with
    // several domains sharing one admin prompts / calls `rbw` only once.
    type PwKey = (Option<String>, Option<String>);
    type PwVal = (Option<String>, conninfo::PwSource);
    let mut pw_cache: HashMap<PwKey, PwVal> = HashMap::new();
    let multi = conns.len() > 1;
    let mut sessions = Vec::with_capacity(conns.len());
    for ec in conns {
        let key = (ec.cfg.server.bind_dn.clone(), ec.cfg.server.password_cmd.clone());
        let (password, pw_source) = pw_cache
            .entry(key)
            .or_insert_with(|| get_password(&ec.cfg))
            .clone();
        let mode = resolve_mode(args.write, args.dry_run, &ec);
        // Resolve the optional cn=config admin password (OpenLDAP schema management).
        let config_password = ec.cfg.server.config_bind_dn.as_ref()
            .and(ec.cfg.server.config_password_cmd.as_deref())
            .and_then(|cmd| run_password_cmd(cmd).ok());
        let label = format!("{} · {}", ec.server_label, ec.domain_label);
        match Session::connect(ec.cfg, password, config_password, pw_source, mode, ec.server_label, ec.domain_label) {
            Ok(session) => sessions.push(session),
            // With several connections configured, one bad/unreachable directory must
            // not sink the rest — warn and skip it. (A dedicated "failed connection"
            // rail entry comes with the rail in Increment C.) A lone connection still
            // fails hard, preserving today's single-server behaviour.
            Err(e) if multi => eprintln!("Warning: skipping connection {label}: {e:#}"),
            Err(e) => return Err(e.context(format!("connecting {label}"))),
        }
    }
    if sessions.is_empty() {
        anyhow::bail!("No LDAP connection could be established");
    }

    // Each session carries its own resolved mode (read-only / write / dry-run); the
    // TUI gates writes per the focused connection.
    tui::run(sessions)
}

/// Resolve a connection's effective write posture. Precedence: CLI `--dry-run` /
/// `--write` (global override) > the file's per-`[[domain]]` `mode` > the server's
/// `display.allow_writes` > read-only.
fn resolve_mode(cli_write: bool, cli_dry: bool, ec: &config::EffectiveConfig) -> ConnMode {
    if cli_dry { return ConnMode::DryRun; }
    if cli_write { return ConnMode::Write; }
    if let Some(m) = ec.mode { return m; }
    if ec.cfg.display.allow_writes { return ConnMode::Write; }
    ConnMode::ReadOnly
}

fn get_password(cfg: &Config) -> (Option<String>, conninfo::PwSource) {
    use conninfo::PwSource;
    // No bind DN → anonymous bind, no password needed.
    if cfg.server.bind_dn.is_none() {
        return (None, PwSource::Anonymous);
    }
    // 1. password_cmd in config (e.g. rbw get "...")
    if let Some(cmd) = &cfg.server.password_cmd {
        match run_password_cmd(cmd) {
            Ok(pw) => return (Some(pw), PwSource::Cmd(cmd_label(cmd))),
            Err(e) => eprintln!("Warning: password_cmd failed: {e}"),
        }
    }
    // 2. Environment variable
    if let Ok(pw) = std::env::var("CENSUS_BIND_PASSWORD") {
        return (Some(pw), PwSource::Env);
    }
    // 3. Interactive prompt
    (rpassword::prompt_password("LDAP bind password: ").ok(), PwSource::Prompt)
}

/// The program name of a `password_cmd` (e.g. `rbw get '…'` → `rbw`), for display.
fn cmd_label(cmd: &str) -> String {
    cmd.split_whitespace().next().unwrap_or("cmd").to_string()
}

fn run_password_cmd(cmd: &str) -> anyhow::Result<String> {
    let out = std::process::Command::new("sh")
        .args(["-c", cmd])
        .output()?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        anyhow::bail!("exited {}: {}", out.status, err.trim());
    }
    Ok(String::from_utf8(out.stdout)?.trim().to_string())
}
