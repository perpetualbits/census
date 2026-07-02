//! Cross-directory search: a query field on top, live user/group matches below.
//! Opened with `/` from any screen; `Enter` jumps to the selected entry.

use mullion::{
    label::Align,
    render_field, render_shared,
    table::{ColumnDef, ColumnGrid, ColumnKind},
    Buffer, FieldRender, LineWeight, Node, Rect,
};

use crate::tui::app::{App, HitKind, MAX_HITS};
use crate::tui::draw::{btxt, fill_row, hline, keyhints, vscroll};
use crate::tui::theme::*;

const BODY: u64 = 1;

pub fn render(app: &App, buf: &mut Buffer, area: Rect) {
    if area.width < 24 || area.height < 6 { return; }

    // A single framed pane; render_shared draws the border and hands back the
    // deflated inner rect.
    let mut tree = Node::Tile(BODY);
    let rects = render_shared(buf, &mut tree, area, &box_style(), &[(BODY, LineWeight::Heavy)]);
    let inner = rects.iter().find(|(id, _)| *id == BODY).map(|(_, r)| *r).unwrap_or(area);

    btxt(buf, area.x + 2, area.y, "  census — search  ", s_title());

    // Footer: keyhints on the left, a match count on the right.
    let bottom = area.y + area.height - 1;
    let n = app.search_hits().len();
    let count = if n >= MAX_HITS { format!(" {n}+ matches ") } else { format!(" {n} matches ") };
    match &app.status {
        Some((msg, is_err)) => {
            btxt(buf, area.x + 2, bottom, &format!(" {msg} "),
                 if *is_err { s_err() } else { s_ok() });
        }
        None => {
            let pairs: &[(&str, &str)] =
                &[("type", "filter"), ("↑↓", "move"), ("Enter", "go"), ("Esc", "cancel")];
            let cw = count.chars().count() as u16;
            let hint_w = area.width.saturating_sub(4).saturating_sub(cw);
            keyhints(buf, area.x + 2, bottom, hint_w, pairs);
            let cx = area.x + area.width.saturating_sub(1 + cw);
            btxt(buf, cx, bottom, &count, s_dim());
        }
    }

    // Query line: a `/ ` prompt then the horizontally-scrolling field.
    let prompt = "/ ";
    btxt(buf, inner.x, inner.y, prompt, s_head());
    let px = prompt.chars().count() as u16;
    let field = Rect::new(inner.x + px, inner.y, inner.width.saturating_sub(px), 1);
    let opts = FieldRender {
        style: s_normal(),
        cursor_style: s_sel(),
        mask: None,
        ctx: dctx(),
    };
    let mut scroll = 0;
    render_field(buf, field, app.search_query(), app.search_caret(), &mut scroll, &opts);

    hline(buf, Rect::new(inner.x, inner.y + 1, inner.width, 1));

    // Results (or a hint when there's nothing to show).
    let data = Rect::new(inner.x, inner.y + 2, inner.width, inner.height.saturating_sub(2));
    if data.height == 0 { return; }
    let vis = data.height as usize;

    if app.search_query().trim().is_empty() {
        btxt(buf, data.x, data.y,
             "type a name, account (uid), group, or uid/gid number…", s_dim());
        return;
    }
    if app.search_hits().is_empty() {
        btxt(buf, data.x, data.y, "no matches", s_dim());
        return;
    }

    let cur = &app.search_cur;
    let content = vscroll(buf, data, cur.offset, app.search_hits().len(), vis);
    let cols = row_grid().resolve(content);
    for (i, hit) in app.search_hits().iter().enumerate().skip(cur.offset).take(vis) {
        let y   = content.y + (i - cur.offset) as u16;
        let sel = i == cur.cursor;
        let sty = if sel { s_sel() } else { s_normal() };
        let dim = if sel { s_sel() } else { s_dim() };
        if sel { fill_row(buf, content.x, y, content.width, sty); }

        let tag = match hit.kind { HitKind::User => "user ", HitKind::Group => "group" };
        ColumnGrid::write_text(buf, cols[0], y, tag, Align::Start, dim);
        ColumnGrid::write_text_ctx(buf, cols[2], y, &hit.primary, Align::Start, sty, dctx());
        ColumnGrid::write_text_ctx(buf, cols[4], y, &hit.secondary, Align::Start, dim, dctx());
    }
}

/// tag | gap | primary (uid / group name) | gap | secondary context.
fn row_grid() -> ColumnGrid {
    ColumnGrid::new(vec![
        ColumnDef::fixed(5,  ColumnKind::Text),
        ColumnDef::fixed(1,  ColumnKind::Custom),
        ColumnDef::fixed(18, ColumnKind::Text),
        ColumnDef::fixed(1,  ColumnKind::Custom),
        ColumnDef::fill(1,   ColumnKind::Text).with_min(10),
    ])
}
