//! A tight yellow Gaussian "comet" that travels around the outer border.
//!
//! Each frame we recolour the border cells near a moving hotspot, blending their
//! existing foreground toward yellow with a Gaussian falloff. We drive this through
//! mullion's [`render_rim`]: it walks the perimeter and hands each cell its
//! normalised position plus current style, and our closure decides the new colour —
//! so the glyphs (and the title text in the top border) are preserved, the glow just
//! slides over them.

use mullion::{ease::gaussian, render_rim, style::Color, BorderGap, Buffer, Rect};

/// Glow colour (warm yellow).
const GLOW: (f32, f32, f32) = (255.0, 210.0, 40.0);
/// Hotspot width in cells (smaller = tighter).
const SIGMA: f32 = 2.4;
/// Seconds for one full lap of the perimeter.
const LOOP_SECS: f32 = 6.0;
/// Skip cells dimmer than this to avoid touching the whole border.
const CUTOFF: f32 = 0.06;

/// Draw the travelling glow over `area`'s border. `t` is elapsed seconds. Cells
/// inside any of `gaps` (e.g. the connection info gap) are left untouched so their
/// own colours show through.
pub fn edge_glow(buf: &mut Buffer, area: Rect, t: f32, gaps: &[BorderGap]) {
    if area.width < 4 || area.height < 4 {
        return;
    }
    let perim = area.border_len() as f32;
    if perim <= 0.0 {
        return;
    }

    // Hotspot position as a fraction of the perimeter, advancing clockwise.
    let head = (t / LOOP_SECS).rem_euclid(1.0);

    render_rim(buf, area, gaps, |pos, cur| {
        // Shortest wrap-around arc to the hotspot, then back to cell units so SIGMA
        // keeps its "width in cells" meaning.
        let mut d = (pos - head).abs();
        d = d.min(1.0 - d);
        let intensity = gaussian(d * perim, SIGMA);
        if intensity < CUTOFF {
            return None;
        }
        Some(cur.fg(blend(cur.fg, intensity)))
    });
}

/// Blend an existing colour toward the glow yellow by `t` in [0, 1].
fn blend(base: Color, t: f32) -> Color {
    let (br, bg, bb) = match base {
        Color::Rgb(r, g, b) => (r as f32, g as f32, b as f32),
        // Non-RGB bases (Reset/Indexed) blend up from the border tone.
        _ => (70.0, 70.0, 100.0),
    };
    let mix = |a: f32, b: f32| (a + (b - a) * t).round().clamp(0.0, 255.0) as u8;
    Color::Rgb(mix(br, GLOW.0), mix(bg, GLOW.1), mix(bb, GLOW.2))
}
