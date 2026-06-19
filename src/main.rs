mod config;
mod ldap;

use std::path::PathBuf;

use clap::Parser;

use config::Config;
use ldap::LdapClient;

#[derive(Parser, Debug)]
#[command(name = "census", about = "LDAP user and group administration TUI")]
struct Args {
    /// Path to config file (default: ~/.config/census/config.toml)
    #[arg(long, value_name = "FILE")]
    config: Option<PathBuf>,

    /// Allow write operations (add/remove group members, edit attributes)
    #[arg(long)]
    write: bool,

    /// Connect and exit; prints user and group counts. Useful for testing.
    #[arg(long)]
    ping: bool,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    let cfg = Config::load(args.config.as_ref())?;

    let allow_writes = args.write || cfg.display.allow_writes;

    let password = get_password(&cfg);

    if args.ping {
        return cmd_ping(&cfg, password.as_deref(), allow_writes);
    }

    // TUI launch goes here once the UI layer is built.
    eprintln!("TUI not yet implemented — run with --ping to test connectivity.");
    Ok(())
}

fn cmd_ping(cfg: &Config, password: Option<&str>, allow_writes: bool) -> anyhow::Result<()> {
    let mode = if cfg.tunnel.enabled { "tunnel" } else { "direct" };
    let tls  = if cfg.server.use_ssl { "LDAPS" } else if cfg.server.start_tls { "LDAP+STARTTLS" } else { "LDAP" };
    eprintln!("Connecting ({mode}, {tls}) to {}:{} …", cfg.server.host, cfg.server.port);

    let mut client = LdapClient::connect(cfg, password)?;
    client.ping()?;
    eprintln!("Bind OK");

    let users  = client.list_users()?;
    let groups = client.list_groups()?;

    println!("Users  ({}):  {}", users.len(),  users.iter().map(|u| u.uid.as_str()).collect::<Vec<_>>().join("  "));
    println!("Groups ({}):  {}", groups.len(), groups.iter().map(|g| g.name.as_str()).collect::<Vec<_>>().join("  "));

    if allow_writes {
        eprintln!("Write mode ENABLED");
    } else {
        eprintln!("Read-only mode (pass --write to enable modifications)");
    }

    client.close()?;
    Ok(())
}

fn get_password(cfg: &Config) -> Option<String> {
    if cfg.server.bind_dn.is_none() {
        return None;
    }
    if let Ok(pw) = std::env::var("CENSUS_BIND_PASSWORD") {
        return Some(pw);
    }
    rpassword::prompt_password("LDAP bind password: ").ok()
}
