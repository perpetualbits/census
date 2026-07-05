//! Domain backups (LDIF export) on **background threads**, so exporting a large
//! directory never blocks the UI. Each backup opens its own read-only connection from
//! the session's config/secret (nothing LDAP crosses the channel — only progress and a
//! final outcome), streams the whole subtree with Simple Paged Results, and writes RFC
//! 2849 content records to a file. The UI polls for events each frame and shows them in
//! the status line. Several backups can run at once.

use std::io::Write;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};

use crate::config::Config;
use crate::ldap::client::LdapClient;
use super::ldif;

/// How often a running backup reports its running entry count.
const PROGRESS_EVERY: u64 = 20_000;

enum Event {
    Progress { label: String, entries: u64 },
    Done { label: String, outcome: Result<(u64, PathBuf), String> },
}

/// Tracks in-flight backups and surfaces their status. One channel collects events
/// from every worker thread.
pub struct Backups {
    tx: Sender<Event>,
    rx: Receiver<Event>,
    active: usize,
}

impl Backups {
    pub fn new() -> Self {
        let (tx, rx) = channel();
        Backups { tx, rx, active: 0 }
    }

    /// Whether any backup is currently running (drives a rail indicator).
    pub fn active(&self) -> usize { self.active }

    /// Spawn a backup of `base` (this domain's subtree) to `path`, labelled `label`
    /// for status messages. Returns immediately; the worker reports via [`poll`].
    pub fn start(&mut self, cfg: Config, password: Option<String>, base: String, path: PathBuf, label: String) {
        self.active += 1;
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let outcome = run(cfg, password, &base, &path, &label, &tx);
            let _ = tx.send(Event::Done { label, outcome });
        });
    }

    /// Drain worker events (non-blocking); returns a `(message, is_error)` status to
    /// show, if anything happened this frame.
    pub fn poll(&mut self) -> Option<(String, bool)> {
        let mut status = None;
        while let Ok(ev) = self.rx.try_recv() {
            status = Some(match ev {
                Event::Progress { label, entries } =>
                    (format!("backing up {label}… {entries} entries"), false),
                Event::Done { label, outcome } => {
                    self.active = self.active.saturating_sub(1);
                    match outcome {
                        Ok((n, p)) => (format!("backed up {label}: {n} entries → {}", p.display()), false),
                        Err(e)     => (format!("backup {label} failed: {e}"), true),
                    }
                }
            });
        }
        status
    }
}

/// The worker body: connect, stream the subtree, write LDIF. Any error becomes a
/// string (the UI only needs to display it).
fn run(cfg: Config, password: Option<String>, base: &str, path: &PathBuf, label: &str, tx: &Sender<Event>)
    -> Result<(u64, PathBuf), String>
{
    let map = |e: anyhow::Error| format!("{e:#}");
    let mut client = LdapClient::connect(&cfg, password.as_deref()).map_err(map)?;
    let mut w = std::io::BufWriter::new(std::fs::File::create(path).map_err(|e| e.to_string())?);
    writeln!(w, "# census backup of {base}\nversion: 1").map_err(|e| e.to_string())?;

    let mut i = 0u64;
    let n = client.stream_subtree(base, |se| {
        write!(w, "\n{}", ldif::entry_ldif(&se.dn, &se.attrs, &se.bin_attrs))?;
        i += 1;
        if i % PROGRESS_EVERY == 0 {
            let _ = tx.send(Event::Progress { label: label.to_string(), entries: i });
        }
        Ok(())
    }).map_err(map)?;
    w.flush().map_err(|e| e.to_string())?;
    Ok((n, path.clone()))
}
