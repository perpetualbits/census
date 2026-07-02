# Feature requests for mullion — round 2: multilingual/bidi + full admin tools

You are working in `~/git/mullion`. This is the **second** round of engine asks (round 1
is `mullion-asks.md`, now implemented and merged: rounded shared borders, the perimeter
iterator + `render_rim`, `draw_panel`/`Panel`, `Theme` status roles, `line_edit`/
`render_field`, `visible_window`). Round 1 got census onto current mullion. This round is
driven by two things:

1. Taking census (an LDAP admin TUI) from "works" to **multilingual and bidirectional** —
   operators in RTL/mixed-script locales, names in every script.
2. The next target: Astron's **AAA** tooling (OIDC/Keycloak realm/client/role/mapper
   editors) — **many list screens and many forms/dialogs**.

The goal is unchanged: mullion absorbs the *engine-level* boilerplate these apps repeat, so
the apps stay focused on their domain.

## Guardrails — respect mullion's philosophy

mullion is a **content-agnostic tiling + rendering engine**. The non-goals stand: **not a
retained-mode widget toolkit** (no stateful buttons/inputs/focus rings), **the app owns its
`String`/cursor/focus/domain state**, mullion owns only *navigable* state and *rendering*.
Every item below is a **stateless primitive or pure render helper** — free functions over
`&mut Buffer` + `Rect` + `&Style`, `Copy` value types, or consume-`self` `with_*` builders —
exactly like `draw_box`, `render_field`, `visible_window`. If any item strays past the
engine/widget line in your judgement, push back and propose the in-scope version.

For each accepted item: add/extend a `TestBackend` (or buffer) snapshot test, keep every
`examples/*` compiling, and update `docs/mullion-manual.md`.

### Key grounding finding (please verify first)

The **text** engine is bidi-correct, but the **edit & chrome** layer that sits on top of it
is still LTR-only underneath, and there is a real doc/code gap:

- `edit.rs::line_edit` moves the caret with `prev_boundary`/`next_boundary` — pure
  **logical** order. In RTL/mixed text, `Left` should follow **visual** order.
- `edit.rs::render_field` walks graphemes strictly left-to-right — no bidi reorder.
- `table.rs::write_text` truncates by **`char`** (splitting a base+combining or ZWJ-emoji
  cluster right before the `…`), always elides the physical right, and renders with
  `set_string` — **not** `shape_line`. So table cells are **not** bidi-correct, despite the
  manual's §3.3/§6.3 claim that `shape_line` is the single-line primitive a cell uses.

So the bidi items below don't re-invent the text engine — they **extend it into the edit and
chrome layer**, and close that gap.

---

## Part A — Multilingual / bidirectional (lead)

## A1. `TextCtx` — an app-threaded base-direction + digit-shaping context

**Problem.** Every bidi-aware call re-plumbs `BaseDirection`, and chrome primitives
(`draw_label`, `draw_panel`, `ColumnGrid`, `render_field`) can't see it at all. A census/AAA
screen rendered for an Arabic or Hebrew operator needs *one* locale decision to reach labels,
fields, tables, footers, truncation, and digit shaping — not a direction argument threaded
through 40 call sites. This is the keystone the other bidi items build on.

**Proposed primitive** (a `Copy` value the app owns; mullion stores nothing):

```rust
/// Display-only digit shaping (never mutates stored text).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DigitShaping {
    #[default] None,
    ArabicIndic,          // ٠١٢٣٤٥٦٧٨٩  (U+0660…)
    ExtendedArabicIndic,  // ۰۱۲۳۴۵۶۷۸۹  (U+06F0…, Persian/Urdu)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TextCtx {
    pub base:   BaseDirection,   // reuse text::BaseDirection (Ltr/Rtl/Auto)
    pub digits: DigitShaping,
}
impl TextCtx {
    pub const LTR: TextCtx = TextCtx { base: BaseDirection::Ltr, digits: DigitShaping::None };
    pub fn rtl() -> Self { Self { base: BaseDirection::Rtl, ..Default::default() } }
}

/// Display-only digit substitution. Arabic-Indic digits are width-1, so display
/// width is preserved — safe to apply after width math. Pure `&str -> Cow`.
pub fn shape_digits(s: &str, shaping: DigitShaping) -> std::borrow::Cow<'_, str>;
```

**In-scope:** a plain `Copy` value passed by argument — the same category as `Style`. No
state, no retained locale. Builds on `text::BaseDirection`. **Effort: S.**

---

## A2. Bidi-correct caret motion for fields — `visual_step`

**Problem.** `line_edit`'s `Left`/`Right` step logically. In census's attribute editor or a
typed-DN confirmation holding Arabic, `Left` must move to the grapheme that is *visually* to
the left — a different logical byte depending on the run's embedding level. Insert/delete stay
logical (correct); only motion must go through the `CursorMap`, which mullion already owns and
apps cannot re-derive without re-implementing UAX #9.

**Proposed primitives** (pure functions; the app still owns the `String` and byte cursor):

```rust
/// Move a logical byte-cursor one grapheme in VISUAL space across a bidi-shaped line.
/// `Left`/`Right` are physical arrow directions. Returns the new byte cursor, or `None`
/// at the visual edge (the signal a form uses to move focus — mirrors `line_edit`'s `false`).
pub fn visual_step(text: &str, cursor: usize, dir: Direction, ctx: TextCtx) -> Option<usize>;

/// The caret's visual column, and the byte cursor nearest a visual column (mouse click,
/// visual Home/End).
pub fn caret_visual_col(text: &str, cursor: usize, ctx: TextCtx) -> u16;
pub fn caret_from_visual_col(text: &str, col: u16, ctx: TextCtx) -> usize;
```

Internally: `shape_line` → `map.logical_to_visual` → step ±1 → `map.visual_to_logical` →
`source_byte`. Document the direction-boundary rule ("prefer the position on the run the caret
is leaving") and lock it with a snapshot test. **In-scope:** read-only projection through
`CursorMap`, like `render_line`. Builds on `text::shape_line` + `CursorMap`. **Effort: S/M.**

---

## A3. Bidi-aware `render_field` (reorder + digit shaping)

**Problem.** `render_field` places graphemes left-to-right in logical order and scrolls in
logical columns, so any RTL content renders reversed with the wrong scroll-to-cursor. This is
the render half of A2 — together they make a single-line field usable in Arabic/Hebrew, table
stakes for AAA's user/realm editors.

**Proposed change** — route `render_field` through `shape_line`, add the context:

```rust
pub struct FieldRender {
    pub style:        Style,
    pub cursor_style: Style,
    pub mask:         Option<char>,
    pub ctx:          TextCtx,   // NEW
}
// signature unchanged; body reorders cells via shape_line, resolves the cursor cell through
// CursorMap, computes the scroll window in VISUAL columns (A2 + visible_window), and applies
// shape_digits to the visible run. Masking collapses to LTR, so the bidi path is inert there.
pub fn render_field(buf, rect, text, cursor, scroll: &mut usize, opts: &FieldRender);
```

**In-scope:** same signature and statelessness (`scroll` stays caller-owned); it moves
`render_field` onto `shape_line`, closing the doc/code gap. **Effort: M** (~40-line rewrite of
the placement loop; snapshot-lockable).

---

## A4. Grapheme+width-correct, direction-aware truncation — `elide` (+ route table cells through `shape_line`)

**Problem.** `ColumnGrid::write_text` truncates by `char`, always elides the physical right,
and bypasses `shape_line` — so census's DN/attribute lists and AAA's client/role lists split
clusters at the ellipsis, mis-measure CJK, and aren't bidi-correct.

**Proposed primitive** — a shared, grapheme-correct, direction-aware elider:

```rust
/// Truncate `text` to at most `max_cols` display columns, grapheme-correctly, with a
/// single-width `…` on the elided side. Under an RTL base the ellipsis and kept run flip.
/// Returns the shaped, clipped `VisualLine` (so the cell↔source map survives) for `render_line`.
pub fn elide(text: &str, max_cols: u16, ctx: TextCtx) -> VisualLine;
```

Then give `ColumnGrid` a `with_ctx(ctx)` builder so a whole grid inherits one direction, and
have `write_text` use `elide` + `render_line` internally — making the manual's claim true.
`write_number` gets `shape_digits`. **In-scope:** `elide` is the truncating sibling of
`shape_line`, pure. **Effort: S/M.** *Flag:* adding `ctx` to `ColumnGrid`/`write_text` is a
mild breaking change — pre-1.0, and consistent with round-1's `render_shared` unification.

---

## A5. RTL layout mirroring — logical `Align`, mirrored columns, scrollbar side, pane order

**Problem.** A full RTL screen doesn't just reverse text — the **layout** mirrors: column order
runs right-to-left, `Align::Start`/`End` mean the *leading*/*trailing* edge, the scrollbar
moves to the left gutter, the marquee reverses, split/pane order flips. Today `Align` is
hard-physical and nothing consults direction. This is pure geometry — squarely mullion's job.

**Proposed primitives** (pure resolvers; the app opts a subtree into mirroring):

```rust
/// Physical anchor after resolving a logical Align against a base direction.
/// LTR: Start→Left, End→Right. RTL: Start→Right, End→Left. Center invariant.
pub enum Anchor { Left, Center, Right }
impl Align { pub fn resolve(self, base: BaseDirection) -> Anchor; }

/// Which gutter a scrollbar occupies (Left for RTL, Right for LTR).
pub fn scrollbar_side(base: BaseDirection) -> Side;

/// Reverse already-solved sibling rects within `area` in place — mirrors column order
/// (ColumnGrid) or pane order (a solved Split row) about `area`'s vertical axis,
/// preserving each rect's width. Pure geometry over the solver's own output.
pub fn mirror_rects_in(area: Rect, rects: &mut [Rect]);

impl ColumnGrid { pub fn mirrored(self, base: BaseDirection) -> Self; }
```

`draw_label`/`draw_panel` resolve title/footer alignment through `Align::resolve`; the marquee
advances in the base reading direction. **In-scope:** pure functions over `Rect`/`Align`/`Side`,
the family of `frame_tiles`/`region_of`; no new `Node` variant, no state. **Effort: M.**

---

## Part B — Full-featured admin tools

## B1. Multi-line / textarea editing (bidi) — the biggest admin boilerplate

**Problem.** OIDC JSON policy blobs, SAML metadata, SSH keys, PEM certs, LDIF bodies — every
AAA/census dialog that isn't a one-liner needs a **wrapped, multi-line** editor: 2-D caret
motion (`Up`/`Down` visual, `Home`/`End` per visual line, `Enter` inserts newline), vertical
scroll, bidi correctness. `line_edit` is single-line only; mullion owns wrapping + the per-line
`CursorMap`, so apps cannot re-derive 2-D bidi motion without re-implementing the engine.

**Proposed primitives** (app owns the `String`, byte cursor, scroll-top, and goal-column):

```rust
/// Pure key→edit transform over caller-owned multi-line state. `width`/`ctx` are needed
/// because Up/Down are defined on the WRAPPED layout. Returns true if consumed; false at an
/// edge (Up on line 0) so a form re-routes it.
pub fn textarea_edit(text: &mut String, cursor: &mut usize, key: KeyCode, width: u16, ctx: TextCtx) -> bool;

/// Render wrapped text with vertical scroll-to-cursor (caller-owned `scroll_top`, via
/// visible_window) and the cursor cell styled. Bidi per line.
pub fn render_textarea(buf: &mut Buffer, area: Rect, text: &str, cursor: usize, scroll_top: &mut usize, opts: &FieldRender);
```

The `Up`/`Down` goal-column is a **caller-owned field** (like `scroll`), not engine state, so
mullion stays stateless. Builds on `text::{wrap, CursorMap, render_wrapped}` + A2. **Effort: L.**
*Flag:* the largest surface and closest to "an editor." *For:* the non-goal is a *retained
widget with its own state/events*; this is a stateless transform + render pass, same category
as the merged `line_edit`. *Against:* goal-column/undo tempt scope creep — draw the line at
**no undo stack, no clipboard, no selection inside this primitive** (selection is B6).

---

## B2. Stateless form layout — label:field rows, tab-order arithmetic, inline validation

**Problem.** census's 9-field new-user form, new-group, both password fields, and every AAA
realm/client/mapper dialog repeat the same layout: a column of `label : [field]` rows,
per-row validation status, and `Tab`/`BackTab` order arithmetic with wraparound. Pure layout
plus a modulo, re-hand-rolled per dialog.

**Proposed primitives** (app owns the `Vec<Field>` and which index is focused):

```rust
pub struct FormRow { pub label: Rect, pub field: Rect, pub status: Rect }
pub struct FormLayout { pub label_cols: u16, pub gap: u16, pub status_cols: u16, pub row_height: u16 }
impl FormLayout {
    /// Resolve `n` stacked rows in `area`, direction-mirrored under `ctx`.
    pub fn rows(&self, area: Rect, n: usize, ctx: TextCtx) -> Vec<FormRow>;
}

/// Pure tab-order step with wraparound (Tab→+1, BackTab→-1). No focus state held.
pub fn focus_step(n: usize, current: usize, dir: Direction) -> usize;

pub enum Validity { Ok, Warn(&'static str), Error(&'static str), None }
pub fn render_validity(buf: &mut Buffer, status: Rect, v: &Validity, theme: &Theme);
```

**In-scope:** `FormLayout::rows` is `ColumnGrid` on the vertical axis (pure `solve`); `focus_step`
is arithmetic; `render_validity` is a themed glyph. Builds on `layout::solve`,
`Theme::{ok,warn,error}`, A5. **Effort: M.**

---

## B3. Key-hint / command footer bar — `render_keyhints`

**Problem.** Every census and AAA screen hand-draws a footer like `Enter save · Esc cancel ·
Tab next`. `draw_panel`'s `footer` is a single centred string — no per-key styling, no
truncation, no direction ordering. The single most-repeated line across all screens.

```rust
/// Lay out `(key, label)` pairs across `rect`: keys in `theme.accent`, labels in
/// `theme.text_dim`, ` · ` separators, grapheme-correct elision on overflow. Order and
/// alignment resolve through `ctx` (RTL reads right-to-left).
pub fn render_keyhints(buf: &mut Buffer, rect: Rect, hints: &[(&str, &str)], theme: &Theme, ctx: TextCtx);
```

**In-scope:** pure render helper over a slice; composes with `draw_panel`'s interior. Builds on
`Theme`, A4 `elide`, A5. **Effort: S — highest leverage-per-line here.**

---

## B4. Tree / outline view — flatten + guide glyphs

**Problem.** census's LDAP DIT browser and AAA's role/group hierarchies need an indented,
collapsible outline with `├─ └─ │` guides. A *retained tree widget* is out of scope, but the
mechanical part — guide glyphs for a caller-flattened row — is pure and re-derived per app.

```rust
/// The guide prefix for a row at depth `ancestor_last.len()`, where `ancestor_last[i]` is
/// true when the ancestor at level i is its parent's last child ("│" vs " "). ASCII-fallback
/// aware via box_to_ascii. `expanded` renders ▸/▾/none.
pub fn tree_prefix(ancestor_last: &[bool], is_last: bool, expanded: Option<bool>) -> String;

/// Draw one outline row: guide prefix (text_dim) + label (bidi via shape_line), selection
/// background across the row when `selected`. Guides mirror under RTL (├→┤).
pub fn render_tree_row(buf: &mut Buffer, rect: Rect, ancestor_last: &[bool], is_last: bool,
                       expanded: Option<bool>, label: &str, selected: bool, theme: &Theme, ctx: TextCtx);
```

**In-scope:** mullion owns guides + navigable state (scroll via `visible_window`); the **domain
tree and expand-set stay app-owned**. Builds on `shape_line`, `Theme`, `charset::box_to_ascii`.
**Effort: M.**

---

## B5. Before/after diff — line LCS + unified render

**Problem.** census wants an **LDIF change preview** (already built app-side, would happily
adopt this); AAA wants realm/client config diffs before commit. Every such preview needs a
line diff and a themed add/remove render.

```rust
pub enum DiffOp<'a> { Equal(&'a str), Insert(&'a str), Delete(&'a str) }

/// Myers/LCS line diff over two slices of lines. Pure; allocation-bounded by input.
pub fn diff_lines<'a>(old: &'a [&'a str], new: &'a [&'a str]) -> Vec<DiffOp<'a>>;

/// Render ops into `area`: unified (`+`/`-`/` ` gutter, ok/error styling) with vertical
/// scroll-to via caller-owned `scroll_top`.
pub fn render_diff_unified(buf: &mut Buffer, area: Rect, ops: &[DiffOp], scroll_top: &mut usize, theme: &Theme, ctx: TextCtx);
```

**In-scope (argue it):** `diff_lines` is a pure text algorithm — the category of `wrap`'s UAX
passes — content-agnostic over `&[&str]`, high-repeat across both apps. *Flag:* if the team
wants a harder engine/domain line, ship only `render_diff_unified` (taking caller-computed
`DiffOp`s) as the primary API and leave the algorithm to the app. **Recommendation: ship both,
render helper as the primary surface.** Builds on `shape_line`, `Theme`. **Effort: M/L.**

---

## B6. Bidi line selection + copy — `selection_step` (+ optional OSC-52 backend emit)

**Problem.** Operators need to **select and copy** DNs, client secrets, tokens. Selection over
bidi text is subtle: a contiguous *logical* range maps to a possibly-**discontiguous visual**
span — the manual names this as correct; mullion carries the map to do it right and apps
cannot.

```rust
/// Extend a selection one grapheme visually (Shift+arrow); anchor stays put, app owns both ends.
pub fn selection_step(text: &str, caret: usize, dir: Direction, ctx: TextCtx) -> Option<usize>;

/// Render a line highlighting the visual cells whose source_byte ∈ sel — a direction-crossing
/// selection highlights the correct (possibly split) visual span.
pub fn render_line_selected(buf: &mut Buffer, rect: Rect, text: &str,
                            sel: std::ops::Range<usize>, style: Style, sel_style: Style, ctx: TextCtx);
```

Copying bytes is trivial (`&text[sel]`); pushing to the OS clipboard is not mullion's job, but
offer it where the backend already owns the terminal:

```rust
impl CrosstermBackend { pub fn copy_to_clipboard(&mut self, text: &str) -> io::Result<()>; } // OSC 52
```

**In-scope:** selection model + render are pure `CursorMap` projections (the read-only twin of
`visual_step`). *Flag:* the OSC-52 write is I/O — correctly placed on the backend, not a widget,
and unsupported terminals ignore the escape (same posture as the existing BDSM `ESC[8l`).
Builds on `CursorMap`, A2, `backend::CrosstermBackend`. **Effort: M.**

---

## Minor / bundle-if-easy (all S)

- **Filter match-highlight.** `highlight_ranges(buf, rect, line, &[Range<usize>], ctx, hit_style)`
  — draws a `shape_line`'d row with matched substrings styled (AAA/census filter boxes).
- **Sort/filter indicators.** `sort_glyph(dir) -> char` (`▲`/`▼`) + a themed filter-active marker
  for table headers.
- **Indeterminate progress.** `spinner_frame(t: f32) -> char` (Braille cycle) +
  `render_indeterminate(buf, rect, t, style)`. Determinate bars are already
  `ColumnGrid::write_bar`, so keep this to the indeterminate case.
- **Toast region.** `render_toast(buf, rect, msg, severity, theme)` — ship only if it reads
  cleaner than composing `draw_panel` + `Theme`.
- **Masked-reveal.** Extend `FieldRender::mask` to `Mask { ch: char, reveal_last: bool }` for
  the "show last typed char" password affordance.

---

### Priority order

Lead with the bidi keystone and the field path, since they unblock multilingual data entry:

1. **A1 `TextCtx`** (S) — keystone; everything else takes it.
2. **A2 `visual_step`** (S/M) + **A3 bidi `render_field`** (M) — single-line fields go multilingual.
3. **A4 `elide`** (S/M) — bidi + cluster-correct table cells; closes the doc/code gap.
4. **A5 RTL mirroring** (M) — unlocks full-RTL screens.
5. **B3 `render_keyhints`** (S), **B2 form layout** (M), **B4 tree/outline** (M) — cheap,
   high-frequency admin wins.
6. **B1 textarea** (L) — the heavy hitter for AAA forms.
7. **B5 diff** (M/L), **B6 selection + copy** (M) — carry the only real scope tensions; gate on
   your read.

Minor items are opportunistic. Files most affected: `src/text.rs`, `src/edit.rs`, `src/table.rs`,
`src/label.rs`, `src/geometry.rs`, `src/theme.rs`, `src/panel.rs`, `src/backend/`.
