//! A tight yellow Gaussian "comet" that travels around the outer border.
//!
//! Each frame we recolour the border cells near a moving hotspot, blending their
//! existing foreground toward yellow with a Gaussian falloff. Only the `style`
//! of each cell is touched, so glyphs (and the title text in the top border)
//! are preserved — the glow simply slides over them.

use mullion::{ease::gaussian, style::Color, Buffer, Rect};

/// Glow colour (warm yellow).
const GLOW: (f32, f32, f32) = (255.0, 210.0, 40.0);
/// Hotspot width in cells (smaller = tighter).
const SIGMA: f32 = 2.4;
/// Seconds for one full lap of the perimeter.
const LOOP_SECS: f32 = 6.0;
/// Skip cells dimmer than this to avoid touching the whole border.
const CUTOFF: f32 = 0.06;

/// Draw the travelling glow over `area`'s border. `t` is elapsed seconds.
pub fn edge_glow(buf: &mut Buffer, area: Rect, t: f32) {
    if area.width < 4 || area.height < 4 {
        return;
    }
    let perim = perimeter(area);
    let n = perim.len();
    if n == 0 {
        return;
    }

    // Hotspot position along the perimeter, in cell units [0, n).
    let head = (t / LOOP_SECS).rem_euclid(1.0) * n as f32;

    for (i, &(x, y)) in perim.iter().enumerate() {
        // Circular distance from this cell to the hotspot.
        let mut d = (i as f32 - head).abs();
        d = d.min(n as f32 - d);

        let intensity = gaussian(d, SIGMA);
        if intensity < CUTOFF {
            continue;
        }

        let cell = buf.get_mut(x, y);
        cell.style.fg = blend(cell.style.fg, intensity);
    }
}

/// Border cell coordinates, clockwise from the top-left, with no duplicates.
fn perimeter(area: Rect) -> Vec<(u16, u16)> {
    let x0 = area.x;
    let y0 = area.y;
    let x1 = area.x + area.width - 1;
    let y1 = area.y + area.height - 1;

    let mut p = Vec::with_capacity(2 * (area.width + area.height) as usize);
    for x in x0..=x1 { p.push((x, y0)); }            // top, →
    for y in (y0 + 1)..=y1 { p.push((x1, y)); }      // right, ↓
    for x in (x0..x1).rev() { p.push((x, y1)); }     // bottom, ←
    for y in ((y0 + 1)..y1).rev() { p.push((x0, y)); } // left, ↑
    p
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
