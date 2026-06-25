# Feature requests for mullion — driven by building admin/CRUD TUIs

You are working in `~/git/mullion`. These requests come from rebuilding **census**
(an LDAP admin TUI) onto current mullion, and from the next target: Astron's AAA
infrastructure tooling (OIDC, Keycloak, and more) — which means **many list screens
and many forms/dialogs**. The goal is for mullion to absorb the *engine-level*
boilerplate those apps repeat, so the apps shrink and stay focused on their domain.

## Guardrails — respect mullion's philosophy

mullion is a **content-agnostic tiling + rendering engine**. The roadmap's explicit
non-goals stand: **not a retained-mode widget toolkit** (no stateful buttons/inputs/
focus rings), **the app owns its content and domain state**, mullion owns only
*navigable* state (focus/scroll/zoom). Every request below is therefore framed as a
**stateless primitive or pure render helper** — free functions over `&mut Buffer` +
`Rect` + `&Style`/`&BorderStyle`, or consume-`self` `with_*` builders, exactly like
`draw_box`, `draw_label`, `render_scrollbar`, `ColumnGrid::write_text`. If any item
strays past the engine/widget line in your judgement, push back and propose the
in-scope version. Requirement #5 is explicitly flagged as the one in tension —
treat it as a proposal to accept, reshape, or reject, not a directive.

For each accepted item: add/extend a `TestBackend` snapshot test, keep every existing
`examples/*` compiling, and update `docs/mullion-manual.md` (and the design-note/
roadmap status table where relevant).

---

## 1. `render_shared` must support rounded corners (and fix the manual)

**Problem.** `border.rs::draw_box` and `frame_tiles` take `&BorderStyle`
(`weight` + `corners` + `style`), so they honor `CornerStyle::Rounded` (`╭╮╰╯`).
But `render_shared` takes only `weight: LineWeight` + `style: &Style` and routes
through `junction::resolve`, whose glyph table has **no rounded corner glyphs at all**
(see the "Glyph rules" doc-comment at the top of `junction.rs` — Light/Heavy/Double
arm combinations only). Result: any app that uses the shared-border engine for its
multi-pane layout **cannot have rounded outer corners**. census had rounded corners
via `draw_box`; moving its screens to `render_shared` silently downgraded them to
square `┌┐`. This is the one feature the migration *lost*.

Note rounding only applies to **pure degree-2 Light corners** — i.e. a cell with
exactly two perpendicular Light arms. In a shared-border layout that's precisely the
four outer frame corners; internal divider junctions are tees/crosses (`┬ ┼ ┤`…) and
must stay square. So this is well-defined and safe.

**Proposed change (recommend the unifying one).** Change `render_shared` to take a
`&BorderStyle` instead of `(weight, &Style)`, matching `draw_box`/`frame_tiles`:

```rust
pub fn render_shared(
    buf: &mut Buffer,
    root: &mut Node,
    area: Rect,
    style: &BorderStyle,                 // was: weight: LineWeight, style: &Style
    overrides: &[(TileId, LineWeight)],
) -> Vec<(TileId, Rect)>
```

This is a deliberate breaking change (mullion is pre-1.0; census will absorb it) and
it makes the three border entry points consistent. Then, in `junction::resolve` (or a
post-resolve pass keyed off the requested `CornerStyle`), emit `╭╮╰╯` for cells that
resolve to a pure two-arm **Light** corner when `corners == Rounded`; leave tees,
crosses, heavy, and double untouched (consistent with `border_glyphs`'s existing
"rounded is Light-only, else fall back" rule). The source reader confirmed `EdgeGrid`
is internal, so threading corner intent through is mechanical, not architectural.

**Optional sub-ask.** Let `overrides` optionally carry a full per-tile style (not just
`LineWeight`) so a focused pane can take an accent *color*, not only a heavier weight —
but only if it's clean; weight-only is fine for now.

**Manual bug.** Separately: `docs/mullion-manual.md` §3.3 / the §2 quickstart imply
`render_shared` accepts a `BorderStyle`/`CornerStyle` today. It does not (current
signature is `weight + &Style`). Fix the manual to match reality — and once this
request lands, the unified `&BorderStyle` form makes the docs correct.

---

## 2. A perimeter-cell iterator (and an optional rim-glow helper)

**Problem.** `Rect::border_pos`/`border_len` exist for *smooth* border animations, but
there is **no way to enumerate the discrete perimeter cells** — every app that animates
a border (census's travelling glow) must hand-roll the clockwise top→right→bottom→left
walk with de-duplicated corners. That walk is fiddly and easy to get subtly wrong, and
it duplicates the exact ordering `border_pos` already defines internally.

**Proposed primitive (pure geometry, sits next to `border_pos`/`border_len`).**

```rust
impl Rect {
    /// Border cells clockwise from the top-left, no duplicates — the same walk
    /// order `border_pos` parameterises. Empty for rects smaller than 2×2.
    pub fn border_cells(self) -> impl Iterator<Item = (u16, u16)>;
}
```

This alone deletes census's `perimeter()` helper. With it, the canonical glow becomes:
`for (x,y) in rect.border_cells() { let s = rect.border_pos(x,y); … }`.

**Optional convenience (keep policy in the caller).** A stateless rim pass that walks
the cells, skips `BorderGap`s without `rim_glow`, and hands each cell its `border_pos`
to a caller closure that decides the style — so easing/color stay app-owned and mullion
stays content-agnostic:

```rust
pub fn render_rim(
    buf: &mut Buffer,
    rect: Rect,
    gaps: &[BorderGap],
    style_at: impl Fn(f32 /*border_pos*/, Style /*current*/) -> Option<Style>,
);
```

The iterator is the important half; ship the closure helper only if it reads cleanly.

---

## 3. A stateless panel/dialog chrome helper

**Problem.** census has 7 modal overlays. Every one repeats the same chrome by hand:
clear the interior to the background, `draw_box` a (rounded) frame, write a title over
the top border, write a footer hint over the bottom border, then compute the interior
content rect. The AAA forms (OIDC client editors, realm/role/mapper dialogs) will add
many more. This is pure composition of primitives mullion already has
(`draw_box` + buffer fill + `draw_label`/`Label`/`Side`) — it belongs in the engine as
a render helper, and the `socket.rs` example already draws labelled rounded boxes, so
the direction is consistent.

**Proposed helper (free function, returns the interior — mirrors `frame_tiles`).**

```rust
pub struct Panel<'a> {
    pub border: BorderStyle,
    pub fill:   Option<Style>,   // None = leave contents, Some = clear interior to this
    pub title:  Option<&'a str>, // drawn as a Side::Top label
    pub footer: Option<&'a str>, // drawn as a Side::Bottom label
}

/// Clear+frame `area` per `panel`, draw title/footer over the border via the
/// existing label machinery, and return the interior content rect to paint into.
pub fn draw_panel(buf: &mut Buffer, area: Rect, panel: &Panel) -> Rect;
```

Pairs directly with `FloatLayer`: `solve` → `draw_panel` → fill the returned interior.
Stateless, content-agnostic, no modal *semantics* (focus trapping/backdrop remain the
app's job, per the roadmap) — just the repetitive chrome.

---

## 4. Add status roles to `Theme`

**Problem.** `Theme` covers `border`, `border_focused`, `text`, `text_dim`, `accent`,
`selection` — good, but **no success/warning/error roles**. Admin/AAA tools are
status-heavy (bind OK, write failed, validation error, password mismatch), so every
app redefines these (census has `C_OK`/`C_ERR` + `s_ok`/`s_err`). They're semantic
*roles*, not widgets, so they fit `Theme` cleanly.

**Proposed.** Add `pub ok: Style`, `pub warn: Style`, `pub error: Style` to `Theme`,
populated in `Theme::default()` and `Theme::light()`. (Optional: a distinct `heading`
role — census separates `text` from bold headers/titles.) With status roles present,
census can drop most of its bespoke palette and adopt `Theme` for
border/selection/text/text_dim/accent too — which is the kind of app-side shrink
these requests are aiming for.

---

## 5. ⚠️ SCOPE QUESTION — stateless line-edit + field-render primitives

**This is the highest-leverage item for the apps and the one in tension with the
"no text inputs" non-goal. Judge it; reshape or reject if it crosses the line.**

**Problem.** The single biggest source of boilerplate across census's overlays — and
it will dominate the OIDC/Keycloak forms — is single-line text editing. Every field
(attribute editor, both password fields, the 9-field new-user form, new-group, the
typed-DN confirmation, the SSH-key paste line) re-implements the same logic by hand:
grapheme-aware insert, Backspace/Delete, Left/Right/Home/End, horizontal scroll to keep
the cursor visible, and password masking. mullion *already owns* the hard parts of this
(unicode-segmentation, unicode-width, `CursorMap`, `VisualCell`, `render_line`); apps
are re-deriving grapheme boundaries on top.

**Why this can be in-scope.** The non-goal is a *retained-mode widget* — a thing with
its own state, event loop, and focus ring. The in-scope version is **pure primitives
where the app still owns the `String`, the cursor, the focus, and the form**. mullion
would only contribute grapheme/width correctness and a render pass — i.e. "text
primitives," the same category as `render_line`/`shape_line`.

**Proposed primitives (app owns state; no widget, no focus, no events beyond a pure
key→edit transform):**

```rust
// Pure transform over caller-owned state. Returns true if the key was consumed.
// Grapheme-cluster correct; cursor is a byte index into `text`.
pub fn line_edit(text: &mut String, cursor: &mut usize, key: KeyCode) -> bool;

// Render one line into `rect` with horizontal scroll-to-cursor + optional masking.
// `scroll` is caller-owned (so the app keeps all state); the cursor cell is styled.
pub struct FieldRender {
    pub style:        Style,
    pub cursor_style: Style,
    pub mask:         Option<char>,   // e.g. Some('•') for passwords
}
pub fn render_field(
    buf: &mut Buffer, rect: Rect,
    text: &str, cursor: usize, scroll: &mut usize,
    opts: &FieldRender,
);
```

If even this feels too far, the **minimal** fallback that still helps a lot: expose
grapheme-boundary cursor helpers (next/prev cluster boundary for a `&str` + byte index)
and a "scroll window to show cursor" calculation — leaving all rendering to the app.

**Please decide and tell me which form (full primitives / minimal helpers / none) you'd
accept, with your reasoning** — this is the item where your read on mullion's boundaries
matters most.

---

## Minor / bundle-if-easy

- **Visible-window helper.** A stateless `fn visible_window(cursor: usize, offset:
  &mut usize, len: usize, viewport: usize) -> Range<usize>` capturing the
  keep-cursor-in-view math every list screen repeats (census's `ListCursor::keep_in_view`).
  Small, but every list view needs it. App keeps the cursor/offset; mullion just does
  the windowing arithmetic.

---

### Priority order
1, 2, 3, 4 are clear engine-level wins — do these regardless. 5 is the big
app-shrinker but needs your scope call first. The minor item is opportunistic.
