//! Headless scripting interface: every view and change the TUI offers, as
//! subcommands. Absence of a subcommand launches the TUI (see `main`).
//!
//! Writes go through [`execute`], a faithful mirror of the TUI's `perform()`
//! chokepoint (`tui::app`): `--dry-run` prints the same LDIF the TUI preview shows
//! and sends nothing; otherwise the write is gated on `--write`, applied via the
//! same `LdapClient` methods, and appended to the same on-disk LDIF journal.

mod output;

use std::io::{IsTerminal, Read};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use clap::Subcommand;

use crate::config::Config;
use crate::ldap::client::NewUserSpec;
use crate::ldap::LdapClient;
use crate::schema::Schema;
use crate::tui::app::{describe_action, search_hits};
use crate::tui::journal::Journal;
use crate::tui::ldif::action_ldif;
use crate::tui::overlay::Action;

// ─── command surface ───────────────────────────────────────────────────────────

#[derive(Subcommand, Debug)]
pub enum Command {
    /// Connect, bind, and print user/group counts (connectivity check).
    Ping,
    /// Search users and groups by name, uid, group, or uid/gid number.
    Search {
        query: String,
        #[arg(long)]
        json: bool,
    },
    /// User operations.
    User {
        #[command(subcommand)]
        cmd: UserCmd,
    },
    /// Group operations.
    Group {
        #[command(subcommand)]
        cmd: GroupCmd,
    },
    /// Raw directory-entry operations, addressed by full DN.
    Entry {
        #[command(subcommand)]
        cmd: EntryCmd,
    },
}

#[derive(Subcommand, Debug)]
pub enum UserCmd {
    /// List all users.
    List {
        #[arg(long)]
        long: bool,
        #[arg(long)]
        json: bool,
    },
    /// Show one user's attributes.
    Get {
        uid: String,
        #[arg(long)]
        json: bool,
    },
    /// List the groups a user belongs to.
    Memberships {
        uid: String,
        #[arg(long)]
        json: bool,
    },
    /// Create a user entry.
    Create {
        #[arg(long)]
        uid: String,
        #[arg(long)]
        cn: String,
        #[arg(long)]
        sn: String,
        #[arg(long)]
        given: Option<String>,
        #[arg(long = "uid-number")]
        uid_number: Option<u32>,
        #[arg(long = "gid-number")]
        gid_number: Option<u32>,
        #[arg(long)]
        home: Option<String>,
        #[arg(long)]
        shell: Option<String>,
        #[arg(long)]
        password: Option<String>,
        #[arg(long = "password-stdin")]
        password_stdin: bool,
    },
    /// Delete a user entry.
    Delete {
        uid: String,
        #[arg(long)]
        yes: bool,
    },
    /// Set an attribute on a user (no values clears it).
    Set {
        uid: String,
        attr: String,
        values: Vec<String>,
    },
    /// Set a user's password (via the configured scheme).
    Passwd {
        uid: String,
        #[arg(long)]
        password: Option<String>,
        #[arg(long = "password-stdin")]
        password_stdin: bool,
    },
    /// Manage a user's SSH public keys.
    Key {
        #[command(subcommand)]
        cmd: KeyCmd,
    },
}

#[derive(Subcommand, Debug)]
pub enum KeyCmd {
    /// List a user's SSH public keys.
    List {
        uid: String,
        #[arg(long)]
        json: bool,
    },
    /// Append an SSH key (a literal key line, `@file`, or `-` for stdin).
    Add { uid: String, key: String },
    /// Remove a key by 1-based index or by a substring match (e.g. its comment).
    Remove { uid: String, which: String },
}

#[derive(Subcommand, Debug)]
pub enum GroupCmd {
    /// List all groups.
    List {
        #[arg(long)]
        json: bool,
    },
    /// Show one group.
    Get {
        name: String,
        #[arg(long)]
        json: bool,
    },
    /// List a group's members (uids).
    Members {
        name: String,
        #[arg(long)]
        json: bool,
    },
    /// Create a posixGroup.
    Create {
        #[arg(long)]
        name: String,
        #[arg(long = "gid-number")]
        gid_number: u32,
    },
    /// Delete a group.
    Delete {
        name: String,
        #[arg(long)]
        yes: bool,
    },
    /// Set an attribute on a group (no values clears it).
    Set {
        name: String,
        attr: String,
        values: Vec<String>,
    },
    /// Rename a group (changes its cn/RDN — an LDAP modrdn).
    Rename { name: String, new_name: String },
    /// Add or remove a member.
    Member {
        #[command(subcommand)]
        cmd: MemberCmd,
    },
    /// Add or remove an alias (an extra cn).
    Alias {
        #[command(subcommand)]
        cmd: AliasCmd,
    },
}

#[derive(Subcommand, Debug)]
pub enum MemberCmd {
    Add { name: String, uid: String },
    Remove { name: String, uid: String },
}

#[derive(Subcommand, Debug)]
pub enum AliasCmd {
    Add { name: String, alias: String },
    Remove { name: String, alias: String },
}

#[derive(Subcommand, Debug)]
pub enum EntryCmd {
    /// Show any entry's attributes by DN.
    Get {
        dn: String,
        #[arg(long)]
        json: bool,
    },
    /// List one level of children under a DN.
    Children {
        dn: String,
        #[arg(long)]
        json: bool,
    },
    /// Set an attribute on any entry by DN.
    Set {
        dn: String,
        attr: String,
        values: Vec<String>,
    },
    /// Delete any entry by DN.
    Delete {
        dn: String,
        #[arg(long)]
        yes: bool,
    },
}

// ─── dispatch ──────────────────────────────────────────────────────────────────

/// Write-mode context threaded to every mutation: mirrors the TUI's write flags +
/// the shared LDIF journal.
struct Ctx<'a> {
    write: bool,
    dry_run: bool,
    base_dn: String,
    schema: Schema,
    journal: &'a mut Journal,
}

pub fn dispatch(
    cmd: Command,
    cfg: &Config,
    password: Option<&str>,
    write: bool,
    dry_run: bool,
) -> Result<()> {
    let mut client = LdapClient::connect(cfg, password)?;
    let base_dn = client.base_dn.clone();
    let schema = client.schema().clone();
    let mut journal = Journal::new();
    let mut ctx = Ctx { write, dry_run, base_dn, schema, journal: &mut journal };

    let result = run(&mut client, &mut ctx, cmd, write);
    client.close().ok();
    result
}

fn run(client: &mut LdapClient, ctx: &mut Ctx, cmd: Command, write: bool) -> Result<()> {
    match cmd {
        Command::Ping => ping(client, write),
        Command::Search { query, json } => {
            let (users, _) = client.list_users()?;
            let (groups, _) = client.list_groups()?;
            output::hits(&search_hits(&users, &groups, &query), json);
            Ok(())
        }
        Command::User { cmd } => user(client, ctx, cmd),
        Command::Group { cmd } => group(client, ctx, cmd),
        Command::Entry { cmd } => entry(client, ctx, cmd),
    }
}

fn ping(client: &mut LdapClient, allow_writes: bool) -> Result<()> {
    client.ping()?;
    eprintln!("Bind OK");
    let (users, uc) = client.list_users()?;
    let (groups, gc) = client.list_groups()?;
    println!("users {}{}", users.len(), if uc { "+" } else { "" });
    println!("groups {}{}", groups.len(), if gc { "+" } else { "" });
    if uc || gc {
        eprintln!("NOTE: capped at the browse size limit — more entries exist than shown");
    }
    eprintln!("{}", if allow_writes { "write mode enabled" } else { "read-only" });
    Ok(())
}

// ─── user ────────────────────────────────────────────────────────────────────

fn user(client: &mut LdapClient, ctx: &mut Ctx, cmd: UserCmd) -> Result<()> {
    match cmd {
        UserCmd::List { long, json } => {
            let (users, capped) = client.list_users()?;
            output::users_list(&users, long, json, capped);
            Ok(())
        }
        UserCmd::Get { uid, json } => {
            let u = client.get_user(&uid)?.with_context(|| format!("user {uid} not found"))?;
            output::user(&u, json);
            Ok(())
        }
        UserCmd::Memberships { uid, json } => {
            let (groups, _) = client.list_groups()?;
            let names: Vec<&str> = groups
                .iter()
                .filter(|g| g.members.iter().any(|m| m == &uid))
                .map(|g| g.name.as_str())
                .collect();
            output::string_list(&names, json);
            Ok(())
        }
        UserCmd::Create {
            uid, cn, sn, given, uid_number, gid_number, home, shell, password, password_stdin,
        } => {
            let uidn = match uid_number {
                Some(n) => n,
                None => client.next_uid_number()?,
            };
            let gidn = gid_number.unwrap_or(uidn);
            let home = home.filter(|h| !h.is_empty()).unwrap_or_else(|| format!("/home/{uid}"));
            let shell = shell.filter(|s| !s.is_empty()).unwrap_or_else(|| "/bin/bash".into());
            let pw = resolve_password(password, password_stdin, false)?;
            let spec = NewUserSpec {
                uid,
                cn,
                sn,
                given_name: given.filter(|g| !g.is_empty()),
                uid_number: uidn,
                gid_number: gidn,
                home,
                shell,
                password: pw,
            };
            execute(client, Action::CreateUser(spec), ctx)
        }
        UserCmd::Delete { uid, yes } => {
            let dn = ctx.schema.user_dn(&uid, &ctx.base_dn);
            if ctx.write && !ctx.dry_run {
                confirm_delete(&dn, yes)?;
            }
            execute(client, Action::DeleteEntry { dn, label: uid }, ctx)
        }
        UserCmd::Set { uid, attr, values } => {
            let dn = ctx.schema.user_dn(&uid, &ctx.base_dn);
            execute(client, Action::SetAttr { dn, attr, values }, ctx)
        }
        UserCmd::Passwd { uid, password, password_stdin } => {
            let dn = ctx.schema.user_dn(&uid, &ctx.base_dn);
            let pw = resolve_password(password, password_stdin, true)?
                .expect("required password resolved");
            execute(client, Action::SetPasswd { dn, plaintext: pw }, ctx)
        }
        UserCmd::Key { cmd } => user_key(client, ctx, cmd),
    }
}

fn user_key(client: &mut LdapClient, ctx: &mut Ctx, cmd: KeyCmd) -> Result<()> {
    match cmd {
        KeyCmd::List { uid, json } => {
            let u = client.get_user(&uid)?.with_context(|| format!("user {uid} not found"))?;
            output::string_list(&u.ssh_keys.iter().map(String::as_str).collect::<Vec<_>>(), json);
            Ok(())
        }
        KeyCmd::Add { uid, key } => {
            let dn = ctx.schema.user_dn(&uid, &ctx.base_dn);
            let mut keys =
                client.get_user(&uid)?.with_context(|| format!("user {uid} not found"))?.ssh_keys;
            let k = read_key_arg(&key)?;
            if k.is_empty() {
                anyhow::bail!("empty key");
            }
            keys.push(k);
            execute(client, Action::SetKeys { dn, keys }, ctx)
        }
        KeyCmd::Remove { uid, which } => {
            let dn = ctx.schema.user_dn(&uid, &ctx.base_dn);
            let mut keys =
                client.get_user(&uid)?.with_context(|| format!("user {uid} not found"))?.ssh_keys;
            remove_key(&mut keys, &which)?;
            execute(client, Action::SetKeys { dn, keys }, ctx)
        }
    }
}

// ─── group ───────────────────────────────────────────────────────────────────

fn group(client: &mut LdapClient, ctx: &mut Ctx, cmd: GroupCmd) -> Result<()> {
    match cmd {
        GroupCmd::List { json } => {
            let (groups, capped) = client.list_groups()?;
            output::groups_list(&groups, json, capped);
            Ok(())
        }
        GroupCmd::Get { name, json } => {
            let (groups, _) = client.list_groups()?;
            let g = groups.iter().find(|g| g.name == name)
                .with_context(|| format!("group {name} not found"))?;
            output::group(g, json);
            Ok(())
        }
        GroupCmd::Members { name, json } => {
            let (groups, _) = client.list_groups()?;
            let g = groups.iter().find(|g| g.name == name)
                .with_context(|| format!("group {name} not found"))?;
            output::string_list(&g.members.iter().map(String::as_str).collect::<Vec<_>>(), json);
            Ok(())
        }
        GroupCmd::Create { name, gid_number } => {
            execute(client, Action::CreateGroup { name, gid_number }, ctx)
        }
        GroupCmd::Delete { name, yes } => {
            let dn = group_dn(&ctx.schema, &ctx.base_dn, &name);
            if ctx.write && !ctx.dry_run {
                confirm_delete(&dn, yes)?;
            }
            execute(client, Action::DeleteGroup { dn, name }, ctx)
        }
        GroupCmd::Set { name, attr, values } => {
            let dn = group_dn(&ctx.schema, &ctx.base_dn, &name);
            execute(client, Action::SetAttr { dn, attr, values }, ctx)
        }
        GroupCmd::Rename { name, new_name } => {
            let dn = group_dn(&ctx.schema, &ctx.base_dn, &name);
            execute(client, Action::RenameGroup { dn, new_cn: new_name, old_name: name }, ctx)
        }
        GroupCmd::Member { cmd } => {
            let (name, uid, add) = match cmd {
                MemberCmd::Add { name, uid } => (name, uid, true),
                MemberCmd::Remove { name, uid } => (name, uid, false),
            };
            let group_dn = group_dn(&ctx.schema, &ctx.base_dn, &name);
            let action = if add {
                Action::AddMember { group_dn, uid, group: name }
            } else {
                Action::DelMember { group_dn, uid, group: name }
            };
            execute(client, action, ctx)
        }
        GroupCmd::Alias { cmd } => {
            let (name, alias, add) = match cmd {
                AliasCmd::Add { name, alias } => (name, alias, true),
                AliasCmd::Remove { name, alias } => (name, alias, false),
            };
            let dn = group_dn(&ctx.schema, &ctx.base_dn, &name);
            let action = if add {
                Action::AddAlias { dn, alias, group: name }
            } else {
                Action::RemoveAlias { dn, alias, group: name }
            };
            execute(client, action, ctx)
        }
    }
}

// ─── entry (any DN) ────────────────────────────────────────────────────────────

fn entry(client: &mut LdapClient, ctx: &mut Ctx, cmd: EntryCmd) -> Result<()> {
    match cmd {
        EntryCmd::Get { dn, json } => {
            let attrs = client.read_entry_display(&dn)?;
            output::attrs(Some(&dn), &attrs, json);
            Ok(())
        }
        EntryCmd::Children { dn, json } => {
            let (kids, capped) = client.list_children(&dn)?;
            output::children(&kids, json, capped);
            Ok(())
        }
        EntryCmd::Set { dn, attr, values } => {
            execute(client, Action::SetAttr { dn, attr, values }, ctx)
        }
        EntryCmd::Delete { dn, yes } => {
            if ctx.write && !ctx.dry_run {
                confirm_delete(&dn, yes)?;
            }
            execute(client, Action::DeleteEntry { dn: dn.clone(), label: dn }, ctx)
        }
    }
}

// ─── the write chokepoint (mirrors app::perform) ───────────────────────────────

fn execute(client: &mut LdapClient, action: Action, ctx: &mut Ctx) -> Result<()> {
    // Dry-run first, exactly like perform(): describe + emit the LDIF, send nothing.
    if ctx.dry_run {
        println!("[dry-run] {}", describe_action(&action));
        print!("{}", action_ldif(&action, &ctx.base_dn, &ctx.schema));
        return Ok(());
    }
    if !ctx.write {
        anyhow::bail!("refusing to modify without --write (use --dry-run to preview)");
    }
    cli_apply(client, &action)?;
    let header = format!("epoch {} | {} | {}", now_secs(), whoami(), describe_action(&action));
    let _ = ctx.journal.record(&header, &action_ldif(&action, &ctx.base_dn, &ctx.schema));
    println!("{}", describe_action(&action));
    Ok(())
}

/// Action → `LdapClient` calls — the same mapping the TUI's `apply()` uses (minus
/// the UI cache/selection bookkeeping). `RestoreEntry` is undo-only and never built
/// by the CLI.
fn cli_apply(client: &mut LdapClient, action: &Action) -> Result<()> {
    use Action::*;
    match action {
        SetAttr { dn, attr, values } => {
            let refs: Vec<&str> = values.iter().map(String::as_str).collect();
            client.modify_replace(dn, attr, &refs)
        }
        SetKeys { dn, keys } => client.ssh_key_replace(dn, keys),
        AddMember { group_dn, uid, .. } => client.group_add_member(group_dn, uid),
        DelMember { group_dn, uid, .. } => client.group_remove_member(group_dn, uid),
        SetPasswd { dn, plaintext } => client.set_password(dn, plaintext),
        CreateUser(spec) => client.add_user(spec).map(|_| ()),
        DeleteEntry { dn, .. } => client.delete_entry(dn),
        CreateGroup { name, gid_number } => client.add_group(name, *gid_number, &[]).map(|_| ()),
        DeleteGroup { dn, .. } => client.delete_entry(dn),
        RenameGroup { dn, new_cn, .. } => client.rename_entry(dn, new_cn).map(|_| ()),
        RemoveAlias { dn, alias, .. } => client.modify_delete(dn, "cn", &[alias.as_str()]),
        AddAlias { dn, alias, .. } => client.modify_add(dn, "cn", &[alias.as_str()]),
        RestoreEntry { .. } => anyhow::bail!("restore is undo-only; not available in the CLI"),
    }
}

// ─── helpers ───────────────────────────────────────────────────────────────────

fn group_dn(schema: &Schema, base_dn: &str, name: &str) -> String {
    format!("{}={},{},{}", schema.cn, name, schema.group_ou, base_dn)
}

/// Resolve a password from `--password`, `--password-stdin`, or an interactive
/// prompt (TTY only). When `required` and none is available, error.
fn resolve_password(direct: Option<String>, stdin: bool, required: bool) -> Result<Option<String>> {
    if let Some(p) = direct {
        return Ok(Some(p));
    }
    if stdin {
        let mut s = String::new();
        std::io::stdin().read_line(&mut s)?;
        return Ok(Some(s.trim_end_matches(['\n', '\r']).to_string()));
    }
    if std::io::stdin().is_terminal() {
        return Ok(Some(rpassword::prompt_password("Password: ")?));
    }
    if required {
        anyhow::bail!("no password (use --password, --password-stdin, or run interactively)");
    }
    Ok(None)
}

/// A destructive delete needs `--yes`, or a typed-DN confirmation on a TTY.
fn confirm_delete(dn: &str, yes: bool) -> Result<()> {
    if yes {
        return Ok(());
    }
    if std::io::stdin().is_terminal() {
        use std::io::Write;
        eprint!("Type the full DN to confirm deleting it:\n  {dn}\n> ");
        std::io::stderr().flush().ok();
        let mut line = String::new();
        std::io::stdin().read_line(&mut line)?;
        if line.trim() == dn {
            return Ok(());
        }
        anyhow::bail!("DN did not match; aborting");
    }
    anyhow::bail!("refusing to delete {dn} without --yes")
}

/// A key argument is a literal key line, `@path` (read a file), or `-` (read stdin).
fn read_key_arg(arg: &str) -> Result<String> {
    let raw = if arg == "-" {
        let mut s = String::new();
        std::io::stdin().read_to_string(&mut s)?;
        s
    } else if let Some(path) = arg.strip_prefix('@') {
        std::fs::read_to_string(path).with_context(|| format!("reading key file {path}"))?
    } else {
        arg.to_string()
    };
    Ok(raw.trim().to_string())
}

/// Remove a key by 1-based index or by a substring match (e.g. its comment).
fn remove_key(keys: &mut Vec<String>, which: &str) -> Result<()> {
    if let Ok(idx) = which.parse::<usize>() {
        if idx == 0 || idx > keys.len() {
            anyhow::bail!("index {idx} out of range (1..={})", keys.len());
        }
        keys.remove(idx - 1);
        return Ok(());
    }
    match keys.iter().position(|k| k.contains(which)) {
        Some(i) => {
            keys.remove(i);
            Ok(())
        }
        None => anyhow::bail!("no key matching {which:?}"),
    }
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

fn whoami() -> String {
    std::env::var("USER").unwrap_or_else(|_| "unknown".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ldap::client::User;
    use crate::schema::Schema;

    #[test]
    fn group_dn_uses_schema() {
        let s = Schema::rfc2307();
        assert_eq!(
            group_dn(&s, "dc=example", "admins"),
            "cn=admins,ou=groups,dc=example"
        );
    }

    #[test]
    fn remove_key_by_index_is_one_based() {
        let mut keys = vec!["a".into(), "b".into(), "c".into()];
        remove_key(&mut keys, "2").unwrap();
        assert_eq!(keys, vec!["a".to_string(), "c".to_string()]);
    }

    #[test]
    fn remove_key_by_substring() {
        let mut keys = vec!["ssh-ed25519 AAA laptop".into(), "ssh-rsa BBB desktop".into()];
        remove_key(&mut keys, "desktop").unwrap();
        assert_eq!(keys, vec!["ssh-ed25519 AAA laptop".to_string()]);
    }

    #[test]
    fn remove_key_out_of_range_and_no_match_error() {
        let mut keys = vec!["only".to_string()];
        assert!(remove_key(&mut keys, "0").is_err());
        assert!(remove_key(&mut keys, "5").is_err());
        assert!(remove_key(&mut keys, "nope").is_err());
        assert_eq!(keys, vec!["only".to_string()]); // unchanged on error
    }

    #[test]
    fn user_serializes_to_json_without_photo() {
        let u = User {
            dn: "uid=x,dc=e".into(),
            uid: "x".into(),
            cn: "X Y".into(),
            sn: "Y".into(),
            given_name: "X".into(),
            uid_number: 1000,
            gid_number: 1000,
            home: "/home/x".into(),
            shell: "/bin/sh".into(),
            ssh_keys: vec![],
            photo: Some(vec![1, 2, 3]),
            attrs: std::collections::HashMap::new(),
        };
        let json = serde_json::to_string(&u).unwrap();
        assert!(json.contains("\"uid\":\"x\""));
        assert!(!json.contains("photo")); // binary field is skipped
    }
}
