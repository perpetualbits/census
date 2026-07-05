//! Whole-domain migration on a **background thread**, so copying a large directory
//! never blocks the UI. A worker opens its own read-only connection to the source and a
//! write connection to each target (from each session's config/secret — no `LdapClient`
//! crosses the channel), streams the source subtree with Simple Paged Results, and, for
//! every entry, rebases its DN onto each target's base-DN and adds it there.
//!
//! Ordering & idempotence: a subtree search returns parents before children in practice,
//! but not by guarantee, so an add that hits `noSuchObject` (parent not created yet) is
//! deferred and replayed in passes until it lands or the pass makes no progress. An entry
//! that already exists on a target (the shared apex + `ou=…` skeleton) is a skip, not a
//! failure. Only progress and a final summary come back over the channel.

use std::sync::mpsc::{channel, Receiver, Sender};

use crate::config::Config;
use crate::ldap::client::LdapClient;
use super::app::rebase_dn;

/// One entry's attributes as raw bytes (text and binary alike), as [`add_raw_rc`] takes.
type RawEntry = Vec<(String, Vec<Vec<u8>>)>;
/// A queue of `(rebased DN, attributes)` awaiting their parent on a target.
type Deferred = Vec<(String, RawEntry)>;

/// How often a running migration reports its running entry count.
const PROGRESS_EVERY: u64 = 5_000;

/// LDAP result codes the migration treats specially.
const RC_ALREADY_EXISTS: u32 = 68; // entryAlreadyExists — target already has it → skip
const RC_NO_SUCH_OBJECT: u32 = 32; // noSuchObject — parent not added yet → defer & retry

/// One migration target: where to connect, and the base-DN source DNs are rebased onto.
pub struct TargetSpec {
    pub cfg: Config,
    pub password: Option<String>,
    pub base: String,
    pub label: String,
}

/// The tally for a finished migration (summed across all targets).
struct Summary {
    added: u64,
    skipped: u64,
    failed: u64,
    first_err: Option<String>,
}

enum Event {
    Progress { label: String, entries: u64 },
    Done { label: String, outcome: Result<Summary, String> },
}

/// Tracks in-flight whole-domain migrations and surfaces their status. One channel
/// collects events from every worker thread.
pub struct Migrations {
    tx: Sender<Event>,
    rx: Receiver<Event>,
    active: usize,
}

impl Migrations {
    pub fn new() -> Self {
        let (tx, rx) = channel();
        Migrations { tx, rx, active: 0 }
    }

    /// Whether any migration is currently running (drives a rail indicator).
    pub fn active(&self) -> usize { self.active }

    /// Spawn a migration of the source subtree `source_base` (connected via `cfg`/`password`)
    /// to every `target`. `label` names the source domain for status messages. Returns
    /// immediately; the worker reports via [`poll`](Self::poll).
    pub fn start(
        &mut self,
        cfg: Config,
        password: Option<String>,
        source_base: String,
        targets: Vec<TargetSpec>,
        label: String,
    ) {
        self.active += 1;
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let outcome = run(cfg, password, &source_base, targets, &label, &tx);
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
                    (format!("migrating {label}… {entries} entries read"), false),
                Event::Done { label, outcome } => {
                    self.active = self.active.saturating_sub(1);
                    match outcome {
                        Ok(s) => {
                            let mut m = format!("migrated {label}: {} added, {} skipped", s.added, s.skipped);
                            if s.failed > 0 {
                                m += &format!(", {} failed — {}", s.failed, s.first_err.unwrap_or_default());
                            }
                            (m, s.failed > 0)
                        }
                        Err(e) => (format!("migration {label} failed: {e}"), true),
                    }
                }
            });
        }
        status
    }
}

/// One target's live connection plus its rebase base and deferred (parent-missing) entries.
struct Live {
    client: LdapClient,
    base: String,
    label: String,
    deferred: Deferred,
}

/// The worker body: connect source + targets, stream the subtree fanning each entry out to
/// every target (rebased), then replay each target's deferred queue. Any error becomes a
/// string (the UI only needs to display it).
fn run(
    cfg: Config,
    password: Option<String>,
    source_base: &str,
    targets: Vec<TargetSpec>,
    label: &str,
    tx: &Sender<Event>,
) -> Result<Summary, String> {
    let map = |e: anyhow::Error| format!("{e:#}");
    let mut source = LdapClient::connect(&cfg, password.as_deref()).map_err(map)?;
    let mut live: Vec<Live> = Vec::with_capacity(targets.len());
    for t in targets {
        let client = LdapClient::connect(&t.cfg, t.password.as_deref())
            .map_err(|e| format!("connect target {}: {e:#}", t.label))?;
        live.push(Live { client, base: t.base, label: t.label, deferred: Vec::new() });
    }

    let mut sum = Summary { added: 0, skipped: 0, failed: 0, first_err: None };
    let mut read = 0u64;
    source.stream_subtree(source_base, |se| {
        // Skip the source apex itself: it's the one entry whose RDN (e.g. `dc=alpha`) is the
        // leading label of the swapped suffix, so rebasing changes the RDN but not the copied
        // `dc:` value — the target would reject it. Its counterpart (the target's own root)
        // always already exists, so there's nothing to copy. Every descendant keeps its RDN.
        if se.dn.eq_ignore_ascii_case(source_base) {
            for _ in 0..live.len() { sum.skipped += 1; }
            read += 1;
            return Ok(());
        }
        // Flatten this entry's attributes to raw bytes once, then fan out to every target.
        let mut raw: Vec<(String, Vec<Vec<u8>>)> = Vec::new();
        for (name, vals) in se.attrs {
            raw.push((name, vals.into_iter().map(String::into_bytes).collect()));
        }
        for (name, vals) in se.bin_attrs {
            raw.push((name, vals));
        }
        for t in live.iter_mut() {
            let new_dn = rebase_dn(&se.dn, source_base, &t.base);
            apply(&mut t.client, &new_dn, &raw, &mut t.deferred, &mut sum);
        }
        read += 1;
        if read % PROGRESS_EVERY == 0 {
            let _ = tx.send(Event::Progress { label: label.to_string(), entries: read });
        }
        Ok(())
    }).map_err(map)?;

    // Replay deferred (parent-missing) entries per target in passes until a pass adds
    // nothing new — then whatever remains is a genuine failure.
    for t in live.iter_mut() {
        loop {
            let batch = std::mem::take(&mut t.deferred);
            if batch.is_empty() { break; }
            let before = sum.added;
            for (dn, raw) in batch {
                apply(&mut t.client, &dn, &raw, &mut t.deferred, &mut sum);
            }
            if sum.added == before {
                // No progress this pass: the survivors will never resolve; count them failed.
                for (dn, _) in t.deferred.drain(..) {
                    sum.failed += 1;
                    sum.first_err.get_or_insert_with(|| format!("unresolved parent for {dn} on {}", t.label));
                }
                break;
            }
        }
    }
    Ok(sum)
}

/// Add one rebased entry to a target, classifying the result: added, skipped (already
/// there), deferred (parent missing → push to `deferred`), or failed.
fn apply(
    client: &mut LdapClient,
    dn: &str,
    raw: &[(String, Vec<Vec<u8>>)],
    deferred: &mut Deferred,
    sum: &mut Summary,
) {
    match client.add_raw_rc(dn, raw) {
        Ok(0) => sum.added += 1,
        Ok(RC_ALREADY_EXISTS) => sum.skipped += 1,
        Ok(RC_NO_SUCH_OBJECT) => deferred.push((dn.to_string(), raw.to_vec())),
        Ok(rc) => {
            sum.failed += 1;
            sum.first_err.get_or_insert_with(|| format!("{dn}: LDAP result {rc}"));
        }
        Err(e) => {
            sum.failed += 1;
            sum.first_err.get_or_insert_with(|| format!("{dn}: {e:#}"));
        }
    }
}
