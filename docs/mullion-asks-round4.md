# Feature requests for mullion — round 4: selection-aware virtualization

You are working in `~/git/mullion`. Rounds 1–3 are implemented and merged; round 3's
row-virtualization work (`record::{RecordSource, Window, VecRecordSource, RangeSource}`,
`vlist::{VirtualList, ScrollMetrics, render_scrollbar, scrollbar_side}`, manual §3.17) is
excellent and exactly the right shape for the IP/DNS tool's millions-of-rows case.

This round comes out of trying to **adopt** that virtualization in census (an LDAP admin
TUI) and finding it doesn't fit census's grain yet — not because the primitive is wrong,
but because census's lists are **selection-driven**, and `VirtualList` models only a
**scroll window**. Two gaps block a clean adoption; a third is a smaller nicety.

Context on why this matters: census currently full-loads users/groups and viewport-windows
them (`ListCursor` + `visible_window` + `render_scrollbar`). That's fine at its scale, but
its LDAP loads are unbounded, so a huge container would hang. We've added a stop-gap size
cap (`sizeLimitExceeded` handled, "capped" markers) — but the real fix is virtualization,
and that needs the items below first.

## Guardrails — unchanged

Content-agnostic tiling + rendering engine, not a retained widget toolkit; the app owns
its domain/selection/scroll state, mullion owns navigable state + rendering; every item is
a stateless primitive or a small addition to an existing value type. Snapshot-test each,
keep `examples/*` compiling, update `docs/mullion-manual.md`.

---

## R1. Selection-aware `VirtualList` — the blocker (lead)

**Problem.** Every admin list is navigated by a **cursor** (the highlighted row that j/k
moves and that Enter/e/D act on), not scrolled by wheel. `VirtualList` today exposes
`scroll_by` + `visible()` with **no notion of a selected row**. And selection can't be a
plain window index, because the window **trims and refills** as it scrolls — the same row's
index within `rows` changes. So a consumer that wants a selection has to track it **by
key**, re-find it in `visible()` every frame, and re-implement keep-in-view against the
window — which is most of what `VirtualList` was supposed to absorb. This is *the* reason
census can't move its lists onto `VirtualList`.

**Proposed additions** (selection tracked by key, kept in the window across refills):

```rust
impl<S: RecordSource> VirtualList<S> {
    /// The selected row's key, if any. Selection is by KEY (stable across window
    /// trim/refill), never a window index.
    pub fn selected_key(&self) -> Option<&S::Key>;
    /// The selected row itself, if it's in the materialized window.
    pub fn selected(&self) -> Option<&S::Row>;

    /// Move the selection one row down/up, fetching across the window edge as needed
    /// and keeping the selected row within the viewport. Returns false at the source
    /// boundary (so a form can move focus), mirroring `line_edit`/`textarea_edit`.
    pub fn select_next(&mut self) -> bool;
    pub fn select_prev(&mut self) -> bool;
    pub fn select_page(&mut self, delta: isize) -> bool;

    /// Seek so `key` is selected and visible (jump-to). No-op if absent.
    pub fn select_key(&mut self, key: &S::Key);

    /// The selected row's offset within `visible()`, for drawing the highlight.
    pub fn selected_visible_row(&self) -> Option<usize>;
}
```

Semantics: the selection is anchored to a **key**; `select_next/prev` move it and pull the
adjacent window in (the same `fetch_after`/`fetch_before` the scroll path uses) so the
cursor never falls out of the materialized window, and keep it inside the viewport (the
`visible_window` policy, now applied to a key-anchored cursor). With this, census's
`ListCursor` + manual keep-in-view + render loop collapse into `list.selected()` /
`list.select_next()` / `list.visible()` — a real reduction, and the door opens to
virtualizing the user/group lists over an LDAP VLV source. **Effort: M.**

---

## R2. Tree/outline virtualization — a windowed node

**Problem.** census's DIT browser (and any role/zone hierarchy) is a **flattened tree
across expanded nodes**. `VirtualList` windows a single stable keyset, which a
flattened-and-mutating tree is not — so it doesn't apply. Yet the real scale risk is
exactly here: one container (`ou=people`, a DNS zone) with millions of children. Today
census fetches **all** of a node's children on expand (`list_children`) — the size cap
just stops it from hanging.

**What would help** (pick the smallest that unblocks — argue the line):

- **(a) Per-node windowing.** A way to virtualize **one node's children** — a
  `RecordSource` per expanded container — that the app composes into its own flattened
  outline. Even just a documented recipe + a helper to interleave a windowed child run
  with `render_tree_row` guides (`tree_prefix`) would do: the app owns the tree; mullion
  windows the one big node.
- **(b) A lazy-children outline model.** A `RecordSource`-of-children hook on an outline
  node, so `render_tree_row` rows for a huge node are pulled from a window with a
  "…N more" affordance, guides intact.

(a) is more in keeping with mullion's "app owns the domain tree" stance; (b) is more
turnkey. Either closes census's only genuinely unbounded surface. **Effort: M/L** —
flag whichever crosses your engine/widget line and ship the other.

---

## R3. Minor / bundle-if-easy (S)

- **`ScrollMetrics` from a plain window.** census draws `render_scrollbar` over an
  in-memory list by hand-building `ScrollMetrics { position: offset/len, extent:
  vis/len, exact: true }`. A tiny constructor —
  `ScrollMetrics::from_window(offset: usize, viewport: usize, total: usize)` — would make
  the common non-virtualized case a one-liner and keep the estimate/exact rule in one
  place. Trivial, and every list view (census's and others') uses it.

---

### Priority order

1. **R1 selection-aware `VirtualList`** (M) — the blocker; unblocks every selection-driven
   admin list (census's user/group lists, the IP/DNS tool's tables) and *reduces* consumer
   code.
2. **R2 tree/outline virtualization** (M/L) — census's only unbounded surface (the DIT);
   the IP/DNS tool likely wants the same for zone hierarchies.
3. **R3 `ScrollMetrics::from_window`** (S) — opportunistic.

Files likely touched: `src/vlist.rs` (R1, R3), `src/outline.rs` + `src/record.rs` (R2).
