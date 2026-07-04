mod cli;
mod config;
mod conninfo;
mod ldap;
mod schema;
mod session;
mod tui;

use std::path::PathBuf;

use clap::Parser;

use config::Config;
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

    let cfg = Config::load(args.config.as_ref())?;

    let allow_writes = args.write || cfg.display.allow_writes;

    let (password, pw_source) = get_password(&cfg);

    // A subcommand runs headless and exits; the legacy `--ping` flag maps to it.
    if let Some(cmd) = args.command {
        return cli::dispatch(cmd, &cfg, password.as_deref(), allow_writes, args.dry_run);
    }
    if args.ping {
        return cli::dispatch(cli::Command::Ping, &cfg, password.as_deref(), allow_writes, args.dry_run);
    }

    let session = Session::connect(&cfg, password.as_deref(), pw_source, "(default)".into())?;
    tui::run(vec![session], allow_writes, args.dry_run, cfg, password)
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
