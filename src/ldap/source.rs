//! LDAP-backed [`mullion::RecordSource`]s that let census window huge lists through
//! a [`mullion::VirtualList`] instead of loading everything.
//!
//! The source holds a **shared browse connection** (`Rc<RefCell<LdapClient>>`,
//! separate from the write/detail client so its borrows never overlap). When the
//! server supports **Server-Side Sort**, each fetch is an independent keyset search
//! (`LdapClient::page_users`) — no server cursor state, constant memory, scales to
//! millions, with an *estimated* scrollbar. Without SSS, ordering can't be pushed to
//! the server, so it degrades to a **client-sorted cache** loaded once (bounded by
//! the size cap): correct and exact-scrollbar, but not unbounded.
//!
//! Errors are swallowed to an empty window (the trait can't return `Result`); the
//! last error is flagged for the screen to surface.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use mullion::{RecordSource, Window};

use super::client::{LdapClient, User};

/// Shared handle to the read-only browse connection.
pub type Browse = Rc<RefCell<LdapClient>>;

/// A windowed source of users, sorted by `uid`.
pub struct UserSource {
    client: Browse,
    /// Set when a fetch failed, so the screen can surface a "browse error" note.
    err: Rc<Cell<bool>>,
    /// Whether the server supports SSS+VLV (windowed keyset paging over millions);
    /// else the capped, client-sorted cache fallback.
    vlv: bool,
    /// Last VLV response: total rows (`exact_len`) and the fetched target's 1-based
    /// position (`approx_position`) — an exact scrollbar.
    count: u64,
    pos: u64,
    /// Client-sorted snapshot used only in the no-VLV fallback (loaded once, capped).
    cache: Option<Vec<User>>,
}

impl UserSource {
    pub fn new(client: Browse, err: Rc<Cell<bool>>, vlv: bool) -> Self {
        Self { client, err, vlv, count: 0, pos: 0, cache: None }
    }

    /// One VLV window around `target` (or the start), swallowing errors to `None` and
    /// caching the response position/count for the scrollbar.
    fn vlv(&mut self, target: Option<&str>, before: i32, after: i32) -> Option<Vec<User>> {
        match self.client.borrow_mut().page_users_vlv(target, before, after.max(0)) {
            Ok((users, pos, count)) => {
                self.pos = pos;
                self.count = count;
                Some(users)
            }
            Err(_) => {
                self.err.set(true);
                None
            }
        }
    }

    /// The client-sorted snapshot (no-VLV fallback), loaded once (bounded by the cap).
    fn cache(&mut self) -> &[User] {
        if self.cache.is_none() {
            let users = match self.client.borrow_mut().list_users() {
                Ok((u, _)) => u,
                Err(_) => {
                    self.err.set(true);
                    Vec::new()
                }
            };
            self.cache = Some(users);
        }
        self.cache.as_deref().unwrap_or(&[])
    }
}

/// The paging key of a user: the browse **sort_key** (the configured sort attribute's
/// value, e.g. a `sortRank`) when the VLV path set it, else the `uid` — so the fallback
/// cache mode (which never sets it) keys by `uid` as before.
fn user_key(u: &User) -> &str {
    if u.sort_key.is_empty() { &u.uid } else { &u.sort_key }
}

impl RecordSource for UserSource {
    type Key = String;
    type Row = User;

    fn key_of(&self, row: &User) -> String {
        user_key(row).to_string()
    }

    fn fetch_after(&mut self, key: Option<String>, n: usize) -> Window<User> {
        if self.vlv {
            // byValue(target) returns [target .. target+after]; drop the target for
            // strict-after. From the start, byOffset offset=1 with after=n-1 gives n.
            let after = if key.is_some() { n as i32 } else { n as i32 - 1 };
            match self.vlv(key.as_deref(), 0, after) {
                Some(users) => {
                    let rows = assemble(users, key.as_deref(), n, false, |u| user_key(u));
                    let boundary = rows.len() < n;
                    Window::new(rows, boundary)
                }
                None => Window::empty(),
            }
        } else {
            let users = self.cache();
            let start = match &key {
                None => 0,
                Some(k) => users.partition_point(|u| &u.uid <= k),
            };
            let end = (start + n).min(users.len());
            Window::new(users[start..end].to_vec(), end == users.len())
        }
    }

    fn fetch_before(&mut self, key: Option<String>, n: usize) -> Window<User> {
        if self.vlv {
            match &key {
                // byValue(target) with before=n returns [target-n .. target]; drop target.
                Some(k) => match self.vlv(Some(k), n as i32, 0) {
                    Some(users) => {
                        let rows = assemble(users, Some(k.as_str()), n, true, |u| user_key(u));
                        let boundary = rows.len() < n;
                        Window::new(rows, boundary)
                    }
                    None => Window::empty(),
                },
                // "last n" isn't needed by VirtualList's scroll/select paths.
                None => Window::empty(),
            }
        } else {
            let users = self.cache();
            let end = match &key {
                None => users.len(),
                Some(k) => users.partition_point(|u| &u.uid < k),
            };
            let start = end.saturating_sub(n);
            Window::new(users[start..end].to_vec(), start == 0)
        }
    }

    fn approx_position(&mut self, key: &String) -> Option<f32> {
        if self.vlv {
            // Exact: the fetched target's position over the total (from the VLV response).
            if self.count > 0 {
                Some((self.pos.saturating_sub(1) as f32 / self.count as f32).clamp(0.0, 1.0))
            } else {
                Some(lexical_fraction(key))
            }
        } else {
            let users = self.cache();
            if users.is_empty() {
                None
            } else {
                Some(users.partition_point(|u| &u.uid < key) as f32 / users.len() as f32)
            }
        }
    }

    fn exact_len(&mut self) -> Option<u64> {
        if self.vlv {
            (self.count > 0).then_some(self.count) // VLV contentCount → exact scrollbar
        } else {
            Some(self.cache().len() as u64) // cache mode knows its length → exact
        }
    }
}

/// Turn a server keyset page (ascending) into a strict window: drop the anchor row
/// (the range filter is inclusive), then keep `n` rows — the first `n` for a forward
/// (`after`) fetch, the last `n` (closest to the anchor) for a `before` fetch.
fn assemble<T>(mut rows: Vec<T>, anchor: Option<&str>, n: usize, before: bool, key: impl Fn(&T) -> &str) -> Vec<T> {
    if let Some(k) = anchor {
        rows.retain(|r| key(r) != k);
    }
    if before {
        if rows.len() > n {
            rows.drain(0..rows.len() - n);
        }
    } else {
        rows.truncate(n);
    }
    rows
}

/// A rough `[0,1)` position of `key` in printable-ASCII lexical order (base-95 over
/// the first three bytes) — enough to drive an *estimated* scrollbar thumb over a
/// set whose length is unknown. Monotonic in the key.
fn lexical_fraction(key: &str) -> f32 {
    let mut f = 0.0f32;
    let mut scale = 1.0f32;
    for b in key.bytes().take(3) {
        scale /= 95.0;
        f += (b.saturating_sub(32).min(94) as f32) * scale;
    }
    f.clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::{assemble, lexical_fraction};

    fn ks(s: &[&str]) -> Vec<String> { s.iter().map(|x| x.to_string()).collect() }
    // Signature must be `&String` to match `assemble`'s `Fn(&T) -> &str` with T=String.
    #[allow(clippy::ptr_arg)]
    fn key(s: &String) -> &str { s.as_str() }

    #[test]
    fn assemble_after_drops_anchor_and_truncates() {
        // server returned uid>=c (inclusive), ascending: c,d,e,f — want 2 strict-after.
        let w = assemble(ks(&["c", "d", "e", "f"]), Some("c"), 2, false, key);
        assert_eq!(w, ks(&["d", "e"]));
    }

    #[test]
    fn assemble_after_from_start_keeps_first_n() {
        let w = assemble(ks(&["a", "b", "c"]), None, 2, false, key);
        assert_eq!(w, ks(&["a", "b"]));
    }

    #[test]
    fn assemble_before_drops_anchor_and_keeps_closest_n() {
        // server returned uid<=f (inclusive), ascending: c,d,e,f — want 2 before f.
        let w = assemble(ks(&["c", "d", "e", "f"]), Some("f"), 2, true, key);
        assert_eq!(w, ks(&["d", "e"]));
    }

    #[test]
    fn assemble_no_anchor_before_keeps_last_n() {
        let w = assemble(ks(&["x", "y", "z"]), None, 2, true, key);
        assert_eq!(w, ks(&["y", "z"]));
    }

    #[test]
    fn lexical_fraction_is_monotonic_and_bounded() {
        assert!(lexical_fraction("aaa") < lexical_fraction("abc"));
        assert!(lexical_fraction("abc") < lexical_fraction("zzz"));
        for k in ["", "a", "quixote", "~~~", "0000"] {
            let f = lexical_fraction(k);
            assert!((0.0..=1.0).contains(&f), "{k:?} → {f}");
        }
    }
}
