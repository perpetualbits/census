//! Cross-domain compare on a **background thread**, so diffing two large directories never
//! blocks the UI. A worker opens a read-only connection to each side, streams both subtrees
//! with Simple Paged Results, and diffs them **by relative DN** (each entry's DN with its
//! own base-DN stripped) so two domains on different suffixes line up. It writes a report
//! file listing entries only on one side or differing between them, and returns a summary.
//!
//! Memory: only the source side is held (a fingerprint per entry keyed by relative DN); the
//! target side streams past it. So peak memory is ~one small record per source entry, not
//! both trees. Comparison uses user attributes only (`*` — operational attrs like
//! `entryUUID`/timestamps aren't fetched, so they never register as spurious differences).

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};

use ldap3::SearchEntry;

use crate::config::Config;
use crate::ldap::client::LdapClient;

/// One side of a comparison: where to connect and the base-DN to strip when relativizing.
pub struct Side {
    pub cfg: Config,
    pub password: Option<String>,
    pub base: String,
    pub label: String,
}

/// The tally for a finished comparison.
struct Report {
    matching: u64,
    differing: u64,
    only_source: u64,
    only_target: u64,
}

enum Event {
    Done { label: String, outcome: Result<(Report, PathBuf), String> },
}

/// Tracks in-flight comparisons and surfaces their status. One channel collects events
/// from every worker thread.
pub struct Comparisons {
    tx: Sender<Event>,
    rx: Receiver<Event>,
    active: usize,
}

impl Comparisons {
    pub fn new() -> Self {
        let (tx, rx) = channel();
        Comparisons { tx, rx, active: 0 }
    }

    /// Whether any comparison is currently running (drives a rail indicator).
    pub fn active(&self) -> usize { self.active }

    /// Spawn a comparison of `source` against `target`, writing the report to `path` and
    /// labelling status messages `label`. Returns immediately; reports via [`poll`](Self::poll).
    pub fn start(&mut self, source: Side, target: Side, path: PathBuf, label: String) {
        self.active += 1;
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let outcome = run(source, target, &path, &tx);
            let _ = tx.send(Event::Done { label, outcome });
        });
    }

    /// Drain worker events (non-blocking); returns a `(message, is_error)` status to show.
    pub fn poll(&mut self) -> Option<(String, bool)> {
        let mut status = None;
        while let Ok(ev) = self.rx.try_recv() {
            let Event::Done { label, outcome } = ev;
            self.active = self.active.saturating_sub(1);
            status = Some(match outcome {
                Ok((r, p)) => (
                    format!(
                        "compared {label}: {} match, {} differ, {} only-source, {} only-target → {}",
                        r.matching, r.differing, r.only_source, r.only_target, p.display()
                    ),
                    r.differing > 0 || r.only_source > 0 || r.only_target > 0,
                ),
                Err(e) => (format!("compare {label} failed: {e}"), true),
            });
        }
        status
    }
}

/// The worker body: fingerprint every source entry by relative DN, then stream the target
/// past that map — classifying each entry and collecting the divergences into a report file.
fn run(source: Side, target: Side, path: &PathBuf, _tx: &Sender<Event>) -> Result<(Report, PathBuf), String> {
    let map = |e: anyhow::Error| format!("{e:#}");
    let mut sc = LdapClient::connect(&source.cfg, source.password.as_deref())
        .map_err(|e| format!("connect source {}: {e:#}", source.label))?;
    let mut tc = LdapClient::connect(&target.cfg, target.password.as_deref())
        .map_err(|e| format!("connect target {}: {e:#}", target.label))?;

    // Source side: relative-DN → (fingerprint, original source DN for reporting).
    let mut src: HashMap<String, (u64, String)> = HashMap::new();
    sc.stream_subtree(&source.base, |se| {
        if let Some(rel) = relativize(&se.dn, &source.base) {
            src.insert(rel.to_lowercase(), (fingerprint(&se), se.dn.clone()));
        }
        Ok(())
    }).map_err(map)?;

    // Target side: classify against the source map, removing matches so the remainder is
    // "only in source". Collect divergent DNs for the report (bounded by the diff size).
    let mut rep = Report { matching: 0, differing: 0, only_source: 0, only_target: 0 };
    let mut only_target: Vec<String> = Vec::new();
    let mut differing: Vec<String> = Vec::new();
    tc.stream_subtree(&target.base, |se| {
        if let Some(rel) = relativize(&se.dn, &target.base) {
            match src.remove(&rel.to_lowercase()) {
                None => { rep.only_target += 1; only_target.push(se.dn.clone()); }
                Some((fp, _)) => {
                    if fp == fingerprint(&se) { rep.matching += 1; }
                    else { rep.differing += 1; differing.push(se.dn.clone()); }
                }
            }
        }
        Ok(())
    }).map_err(map)?;

    let mut only_source: Vec<String> = src.into_values().map(|(_, dn)| dn).collect();
    rep.only_source = only_source.len() as u64;
    only_source.sort();
    only_target.sort();
    differing.sort();

    let mut w = std::io::BufWriter::new(std::fs::File::create(path).map_err(|e| e.to_string())?);
    let write = |w: &mut std::io::BufWriter<std::fs::File>, rep: &Report, only_source: &[String], only_target: &[String], differing: &[String]| -> std::io::Result<()> {
        writeln!(w, "# census compare: {} (source) vs {} (target)", source.label, target.label)?;
        writeln!(w, "# {} match, {} differ, {} only-source, {} only-target",
                 rep.matching, rep.differing, rep.only_source, rep.only_target)?;
        writeln!(w, "\n## only in source ({})", only_source.len())?;
        for dn in only_source { writeln!(w, "< {dn}")?; }
        writeln!(w, "\n## only in target ({})", only_target.len())?;
        for dn in only_target { writeln!(w, "> {dn}")?; }
        writeln!(w, "\n## present on both but differing ({})", differing.len())?;
        for dn in differing { writeln!(w, "! {dn}")?; }
        Ok(())
    };
    write(&mut w, &rep, &only_source, &only_target, &differing).map_err(|e| e.to_string())?;
    w.flush().map_err(|e| e.to_string())?;
    Ok((rep, path.clone()))
}

/// A stable content fingerprint of an entry's **user** attributes: names lowercased, attrs
/// and values sorted, then hashed. Two entries with the same fingerprint have identical
/// attribute sets (modulo attribute-name case and value order).
fn fingerprint(se: &SearchEntry) -> u64 {
    let mut items: Vec<(String, Vec<Vec<u8>>)> = Vec::new();
    for (k, v) in &se.attrs {
        let mut vv: Vec<Vec<u8>> = v.iter().map(|s| s.clone().into_bytes()).collect();
        vv.sort();
        items.push((k.to_lowercase(), vv));
    }
    for (k, v) in &se.bin_attrs {
        let mut vv = v.clone();
        vv.sort();
        items.push((k.to_lowercase(), vv));
    }
    items.sort();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for (k, vv) in &items {
        k.hash(&mut h);
        for v in vv { v.hash(&mut h); }
    }
    h.finish()
}

/// `dn` with `base` stripped from the end — the entry's DN relative to its own naming
/// context, so entries on different suffixes are comparable. `None` for the apex itself
/// (nothing to compare) or a DN not under `base`.
fn relativize(dn: &str, base: &str) -> Option<String> {
    if dn.eq_ignore_ascii_case(base) {
        return None; // the naming-context root — no relative name
    }
    let suffix = format!(",{base}");
    if dn.len() > suffix.len() && dn[dn.len() - suffix.len()..].eq_ignore_ascii_case(&suffix) {
        Some(dn[..dn.len() - suffix.len()].to_string())
    } else {
        None // not under this base (shouldn't occur within a subtree search)
    }
}

#[cfg(test)]
mod tests {
    use super::relativize;

    #[test]
    fn relativize_strips_the_base_and_skips_the_apex() {
        assert_eq!(
            relativize("uid=ada,ou=users,dc=alpha,dc=test", "dc=alpha,dc=test").as_deref(),
            Some("uid=ada,ou=users")
        );
        // The apex has no relative name.
        assert_eq!(relativize("dc=alpha,dc=test", "dc=alpha,dc=test"), None);
        // Base match is case-insensitive.
        assert_eq!(
            relativize("ou=users,DC=Alpha,DC=Test", "dc=alpha,dc=test").as_deref(),
            Some("ou=users")
        );
    }
}
