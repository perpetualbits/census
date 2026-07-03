# Feature requests for mullion — round 3: render/update split + match highlighting

You are working in `~/git/mullion`. This is the **third** round of engine asks. Rounds 1
(`mullion-asks.md`) and 2 (`mullion-asks-round2.md`) are implemented and merged — census now
builds against mullion `main` and consumes the round-2 surface: `TextCtx`/`BaseDirection::Auto`
bidi, `visual_step` + bidi `render_field`, `elide`/`ColumnGrid` ctx, `render_keyhints`,
`render_tree_row`, `textarea_edit`/`render_textarea`, `render_diff_unified`.

Unlike round 2, **this round is short on purpose.** It comes out of one session that built four
features on top of the round-2 surface — data-level bidi values, a multi-line "big edit"
textarea, an actionable LDAP DIT browser, and an incremental cross-directory search screen (`/`
→ live user/group completion, jump on Enter). The headline finding is a compliment: **round 2
was well-scoped.** The search screen, in particular, needed **zero new primitives** — it fell
out of composing `render_shared` (a single `Node::Tile` yields a framed pane + inner rect),
`render_field` (bidi query box), `ColumnGrid`/`write_text_ctx` (result rows), `vscroll`, and
`render_keyhints`. That composition working cleanly is the main data point.

Only two things generated real friction, and one of them turns out to be census's to fix, not
mullion's (recorded below for honesty). So this is **one genuine ask plus a small one**, both
S/M, plus the usual opportunistic minors.

## Guardrails — unchanged

Same philosophy as round 2: content-agnostic tiling + rendering engine, **not** a retained-mode
widget toolkit; the app owns its `String`/cursor/scroll/focus/domain state, mullion owns only
*navigable* state and *rendering*; every item is a stateless primitive or pure render helper. If
an item strays past the engine/widget line in your judgement, push back and propose the in-scope
version. For each accepted item: snapshot test, keep `examples/*` compiling, update
`docs/mullion-manual.md`.

---

## R1. Split scroll-advance from render for `render_field` / `render_textarea` (lead)

**Problem — the one real ergonomic snag this session.** `render_field(.., scroll: &mut usize, ..)`
and `render_textarea(.., scroll_top: &mut usize, ..)` **mutate** caller scroll state *during the
render call*. But a large class of TUIs — census included — render from an **immutable** borrow
of app state (`fn render(app: &App, buf: &mut Buffer)`), with a separate mutable *update* pass
(census's `update_offsets`, which already advances every list cursor via `visible_window`). A
render helper that needs `&mut` fights that shape. Concretely, this session:

- the **textarea** had to wrap its scroll in `Cell<usize>` purely to hand a `&mut` out of a
  `&self` render (`overlay/textarea.rs`);
- the **search field** sidesteps it by passing a throwaway `&mut 0` every frame — which works
  *only because* a single-line field is always cursor-anchored, so re-deriving the window each
  frame is a no-op. A textarea can't rely on that.

This is exactly the seam mullion **already draws elsewhere**: list scroll is advanced by the pure
`visible_window` in the update pass, then the render pass just reads `offset`. Fields and
textareas are the one place that convention isn't available.

**Proposed primitives** — expose the window computation as pure functions, and let render take
scroll **by value**:

```rust
/// Horizontal scroll (in VISUAL columns) that keeps `cursor` visible in a `width`-wide field,
/// given the previous scroll. Pure — the same projection render_field does internally today,
/// lifted out so the caller can own the write (Cell-free) in its update pass.
pub fn field_scroll(text: &str, cursor: usize, width: u16, ctx: TextCtx, prev: usize) -> usize;

/// Vertical scroll-top (in wrapped visual rows) that keeps `cursor` visible in a
/// `width`×`height` textarea, given the previous scroll-top. Builds on wrap + visible_window.
pub fn textarea_scroll_top(text: &str, cursor: usize, width: u16, height: u16,
                           ctx: TextCtx, prev: usize) -> usize;
```

Then let the render helpers accept scroll **by value** (they no longer need to write it):

```rust
pub fn render_field(buf, rect, text, cursor, scroll: usize, opts: &FieldRender);
pub fn render_textarea(buf, rect, text, cursor, scroll_top: usize, opts: &FieldRender);
```

An app then does, in its update pass: `self.q_scroll = field_scroll(&self.q, self.caret, w, ctx,
self.q_scroll);` and, in render: `render_field(buf, r, &self.q, self.caret, self.q_scroll, &opts);`
— no `Cell`, no throwaway `&mut`, and symmetric with how lists already work.

**In-scope:** `field_scroll`/`textarea_scroll_top` are the read-only twins of `visible_window`
(pure `(cursor, width, prev) -> new`); moving render to by-value scroll *removes* state from the
render pass, strictly more aligned with the philosophy. **Effort: S/M.** *Flag:* the by-value
signature change is mildly breaking — pre-1.0, and the same category as round-2's `render_shared`/
`ColumnGrid` ctx breaks. If you'd rather stay additive, ship the two pure fns now and keep the
`&mut` overloads; census will adopt the pure fns immediately and the by-value cleanup can wait.

---

## R2. Match highlighting for filtered / searched rows — promote `highlight_ranges`

**Problem.** Round 2 listed `highlight_ranges` as an opportunistic minor. The search feature makes
it concrete and wanted-now: results list users/groups matched against name/uid/group/gid, but the
rows can't show **where** the query hit — the operator sees *that* `quixote` matched "alonso" but
not *why*. Every filter/search box across census (and AAA's list screens) has the same need.

```rust
/// Draw a shape_line'd, bidi-correct row (like render_line), styling the visual cells whose
/// source byte falls in any `ranges` span with `hit_style`. Ranges are logical byte ranges in
/// `text`; a direction-crossing match highlights the correct (possibly discontiguous) visual
/// cells, via the same CursorMap projection selection uses.
pub fn highlight_ranges(buf: &mut Buffer, rect: Rect, text: &str,
                        ranges: &[std::ops::Range<usize>],
                        base_style: Style, hit_style: Style, ctx: TextCtx);
```

Ideally also a `ColumnGrid` cell variant (`write_text_highlighted(.., ranges)`) so table columns
get it without leaving the grid. The app computes the match ranges (domain logic — census already
knows them from its matcher); mullion just renders them bidi-correctly. **In-scope:** pure render
helper, read-only `CursorMap` projection, the twin of `render_line_selected` from round-2 B6.
**Effort: S.**

---

## Minor / bundle-if-easy (S)

- **`FieldRender` inline prefix.** The search box hand-places a `/ ` prompt then a field in the
  remaining rect. A `FieldRender { prefix: Option<&str> }` (rendered in `text_dim`, excluded from
  the scroll window) would remove that two-step in every filter/command box. Ship only if it reads
  cleaner than the manual composition.
- **Masked-reveal** (carried from round-2 minors, still unimplemented): `FieldRender::mask` →
  `Mask { ch: char, reveal_last: bool }` for the "show the last typed char" password affordance.
- **Empty/hint row convention.** Every list has a "type to search…" / "no matches" state, hand-
  drawn today. Not worth a primitive unless a `render_placeholder(buf, rect, msg, theme, ctx)`
  centring helper proves reusable across `draw_panel` interiors.

---

## Not a mullion ask — recorded for context (census-side)

While designing search I hit a wall that is **census's architecture, not mullion's**, and I'm
noting it so a mullion maintainer isn't tempted to "fix" it in the engine: census models modals
as an `Overlay` enum whose `handle_key` returns an `OverlayResult` that only carries a *write*
`Action`, and overlays never receive `&App`. Search needs to *read* live directory state (to
filter) and produce a *navigation* result (jump to a screen), neither of which fits that channel —
so it became a `Mode` instead of an overlay. That's a census refactor (give overlays a read
borrow + a navigation variant on `OverlayResult`), squarely outside a content-agnostic engine.
Mullion is not implicated.

---

### Priority order

Deliberately small round:

1. **R1 render/update scroll split** (S/M) — the one real friction; removes the `Cell`/`&mut 0`
   dance and aligns fields/textareas with how lists already work. Ship the two pure fns first
   even if you defer the by-value signature change.
2. **R2 `highlight_ranges`** (S) — promotes a round-2 minor now that search exists to consume it.

Minors are opportunistic. Files most affected: `src/edit.rs` (field/textarea scroll),
`src/text.rs` / `src/table.rs` (`highlight_ranges` + cell variant), `src/label.rs`.
