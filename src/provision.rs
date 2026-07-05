//! Running a short shell script on the LDAP *host* — for the one thing no LDAP client
//! can do remotely: create or remove a backend's on-disk directory (OpenLDAP needs it
//! to exist before a new database will serve). census pipes the script to the
//! operator-configured `provision_cmd`, which provides root shell access to the host
//! (e.g. `ssh ldap.example.com sudo bash -s`, `podman exec -i census-ldap bash`).
//!
//! Piping the script on **stdin** (rather than as an argument) sidesteps all the
//! quoting hazards of nesting a command through ssh/sudo/exec layers.

use anyhow::Context;
use std::io::Write;
use std::process::{Command, Stdio};

/// Run `provision_cmd` (via `sh -c`), feeding `script` on its stdin. Returns stdout on
/// success; on failure the error carries the command's stderr.
pub fn run(provision_cmd: &str, script: &str) -> anyhow::Result<String> {
    let mut child = Command::new("sh")
        .arg("-c").arg(provision_cmd)
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
        .spawn().with_context(|| format!("spawning provision_cmd: {provision_cmd}"))?;
    child.stdin.take().context("provision stdin unavailable")?
        .write_all(script.as_bytes()).context("writing the provision script")?;
    let out = child.wait_with_output().context("running provision_cmd")?;
    if !out.status.success() {
        anyhow::bail!(
            "provision command failed ({}): {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}
