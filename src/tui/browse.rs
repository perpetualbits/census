//! The user browse list on a **background thread**, so the main loop never blocks on
//! LDAP. On a huge directory a windowed fetch (server-side sort / VLV) can take
//! seconds; running it here keeps the UI — and the border glow — alive, showing a
//! `loading…` hint, and swaps the new window in when it lands.
//!
//! The worker owns the `VirtualList<UserSource>` and its own read-only browse
//! connection (created on the thread — nothing LDAP crosses the channel). The UI
//! sends navigation [`Cmd`]s and renders the latest [`Snapshot`]; only those two
//! plain data types cross threads.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread::JoinHandle;
use std::time::Duration;

use mullion::{ScrollMetrics, VirtualList};

use crate::config::Config;
use crate::ldap::client::LdapClient;
use crate::ldap::client::User;
use crate::ldap::source::UserSource;

/// A navigation command from the UI to the worker.
enum Cmd {
    Viewport(usize),
    Select(isize),           // rows to move the selection (+down / −up)
    Key(String),             // jump so `uid` is selected
    Rebuild(Option<String>), // refresh after a write, preserving this uid
}

/// The worker's reply: what the screen renders, plus paging state.
#[derive(Clone)]
pub struct Snapshot {
    pub rows: Vec<User>,
    pub selected: Option<usize>, // index within `rows`
    pub metrics: ScrollMetrics,
    pub at_top: bool,
    pub at_bottom: bool,
    pub err: bool,
    /// `false` until the first window has loaded (startup).
    pub ready: bool,
}

impl Snapshot {
    fn empty() -> Self {
        Snapshot {
            rows: Vec::new(),
            selected: None,
            metrics: ScrollMetrics::from_window(0, 0, 0),
            at_top: true,
            at_bottom: true,
            err: false,
            ready: false,
        }
    }
    /// The selected user, if any.
    pub fn selected_user(&self) -> Option<&User> {
        self.selected.and_then(|i| self.rows.get(i))
    }
}

/// Handle to the browse worker: send commands, hold the latest snapshot.
pub struct Browse {
    tx: Sender<Cmd>,
    rx: Receiver<Snapshot>,
    pub snapshot: Snapshot,
    /// A command is in flight (no reply yet) — drives the `loading…` hint.
    pub loading: bool,
    /// The last viewport the UI requested (for page-sized moves).
    viewport: usize,
    _handle: JoinHandle<()>,
}

impl Browse {
    /// Spawn the worker; it connects its own browse session from `cfg`.
    pub fn spawn(cfg: Config, password: Option<String>, viewport: usize) -> Self {
        let (ctx, crx) = channel::<Cmd>();
        let (stx, srx) = channel::<Snapshot>();
        let handle = std::thread::spawn(move || worker(cfg, password, viewport.max(1), crx, stx));
        Browse {
            tx: ctx,
            rx: srx,
            snapshot: Snapshot::empty(),
            loading: true,
            viewport: viewport.max(1),
            _handle: handle,
        }
    }

    /// Pick up any snapshots the worker produced (call every frame — non-blocking).
    pub fn poll(&mut self) {
        while let Ok(s) = self.rx.try_recv() {
            self.snapshot = s;
            self.loading = false;
        }
    }

    fn send(&mut self, cmd: Cmd) {
        if self.tx.send(cmd).is_ok() {
            self.loading = true;
        }
    }

    pub fn set_viewport(&mut self, n: usize) {
        let n = n.max(1);
        if n != self.viewport {
            self.viewport = n;
            self.send(Cmd::Viewport(n));
        }
    }
    pub fn select_next(&mut self) { self.send(Cmd::Select(1)); }
    pub fn select_prev(&mut self) { self.send(Cmd::Select(-1)); }
    pub fn page_down(&mut self) { self.send(Cmd::Select(self.viewport as isize)); }
    pub fn page_up(&mut self) { self.send(Cmd::Select(-(self.viewport as isize))); }
    pub fn select_key(&mut self, uid: &str) { self.send(Cmd::Key(uid.to_string())); }
    pub fn rebuild(&mut self, keep: Option<String>) { self.send(Cmd::Rebuild(keep)); }

    /// The selected user's uid, from the latest snapshot.
    pub fn selected_uid(&self) -> Option<String> {
        self.snapshot.selected_user().map(|u| u.uid.clone())
    }
}

// ─── worker ────────────────────────────────────────────────────────────────────

fn worker(cfg: Config, password: Option<String>, viewport: usize, rx: Receiver<Cmd>, tx: Sender<Snapshot>) {
    // A deliberate per-fetch delay for testing responsiveness against a slow server.
    let delay = std::env::var("CENSUS_BROWSE_DELAY_MS").ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(Duration::from_millis);

    let client = match LdapClient::connect(&cfg, password.as_deref()) {
        Ok(c) => Rc::new(RefCell::new(c)),
        Err(_) => {
            let mut s = Snapshot::empty();
            s.err = true;
            s.ready = true;
            let _ = tx.send(s);
            return;
        }
    };
    let vlv = client.borrow().caps().vlv;
    let err = Rc::new(Cell::new(false));
    let mk = |vp: usize| VirtualList::new(UserSource::new(client.clone(), err.clone(), vlv), vp, 64);
    let mut list = mk(viewport);

    let snap = |list: &mut VirtualList<UserSource>| Snapshot {
        rows: list.visible().to_vec(),
        selected: list.selected_visible_row(),
        metrics: list.scroll_metrics(),
        at_top: list.at_top(),
        at_bottom: list.at_bottom(),
        err: err.get(),
        ready: true,
    };

    if tx.send(snap(&mut list)).is_err() {
        return; // UI already gone
    }

    while let Ok(first) = rx.recv() {
        // Coalesce a burst (e.g. a held `j`) into one fetch, then one snapshot.
        let mut batch = vec![first];
        while let Ok(c) = rx.try_recv() {
            batch.push(c);
        }
        for cmd in coalesce(batch) {
            if let Some(d) = delay {
                if matches!(cmd, Cmd::Select(_) | Cmd::Key(_) | Cmd::Rebuild(_)) {
                    std::thread::sleep(d);
                }
            }
            match cmd {
                Cmd::Viewport(n) => list.set_viewport(n.max(1)),
                Cmd::Select(d) => { list.select_page(d); }
                Cmd::Key(k) => list.select_key(&k),
                Cmd::Rebuild(keep) => {
                    let vp = list.viewport();
                    list = mk(vp);
                    if let Some(k) = keep {
                        list.select_key(&k);
                    }
                }
            }
        }
        if tx.send(snap(&mut list)).is_err() {
            break;
        }
    }
}

/// Fold a command burst: sum consecutive selection moves, keep the last viewport,
/// and preserve jumps/rebuilds in order (they set an absolute position).
fn coalesce(cmds: Vec<Cmd>) -> Vec<Cmd> {
    let mut out = Vec::new();
    let mut sel = 0isize;
    let mut vp: Option<usize> = None;
    for c in cmds {
        match c {
            Cmd::Select(d) => sel += d,
            Cmd::Viewport(n) => vp = Some(n),
            other => {
                if let Some(n) = vp.take() {
                    out.push(Cmd::Viewport(n));
                }
                if sel != 0 {
                    out.push(Cmd::Select(sel));
                    sel = 0;
                }
                out.push(other);
            }
        }
    }
    if let Some(n) = vp {
        out.push(Cmd::Viewport(n));
    }
    if sel != 0 {
        out.push(Cmd::Select(sel));
    }
    out
}
