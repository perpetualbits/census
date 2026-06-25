//! Decode an LDAP `jpegPhoto` and paint it into the terminal as colour cells.
//!
//! mullion's [`Video`] widget does the hard part — resampling an RGB frame onto the
//! cell grid. We use [`Encoding::HalfBlock`] (`▀` with the upper source pixel as
//! foreground and the lower as background): full truecolour at 1×2 pixels per cell,
//! which keeps a face recognisable even at portrait size over an SSH pipe.

use mullion::video::{Encoding, Frame, Video};
use mullion::{Buffer, Rect};
use zune_jpeg::JpegDecoder;

/// Target portrait width in cells; the height follows from the image's aspect.
const TARGET_COLS: u16 = 24;

/// Decode JPEG bytes into a mullion [`Frame`], or `None` if they don't parse.
///
/// Handles the colourspaces a `jpegPhoto` realistically carries: 3-component colour
/// (the common case), single-component greyscale, and 4-component (first three
/// channels used). Any decode error yields `None` — a bad blob simply shows no photo.
pub fn decode(jpeg: &[u8]) -> Option<Frame> {
    let mut dec = JpegDecoder::new(jpeg);
    let pixels = dec.decode().ok()?;
    let info = dec.info()?;
    let (w, h) = (info.width as usize, info.height as usize);
    let n = info.components as usize;
    if w == 0 || h == 0 || n == 0 || pixels.len() < w * h * n {
        return None;
    }
    let rgb: Vec<(u8, u8, u8)> = match n {
        1 => pixels[..w * h].iter().map(|&g| (g, g, g)).collect(),
        3 => pixels.chunks_exact(3).take(w * h).map(|c| (c[0], c[1], c[2])).collect(),
        4 => pixels.chunks_exact(4).take(w * h).map(|c| (c[0], c[1], c[2])).collect(),
        _ => return None,
    };
    if rgb.len() < w * h {
        return None;
    }
    Some(Frame::from_rgb(w, h, rgb))
}

/// The portrait's natural size in cells, preserving the image's aspect ratio.
///
/// A terminal cell is roughly twice as tall as wide, and [`Encoding::HalfBlock`]
/// packs two source rows into one cell, so to keep a `iw×ih` image looking
/// undistorted across `cols` columns the height is `cols · ih / (2 · iw)`.
pub fn portrait_cells(frame: &Frame) -> (u16, u16) {
    let iw = frame.width().max(1) as f32;
    let ih = frame.height().max(1) as f32;
    let cols = TARGET_COLS;
    let rows = ((cols as f32) * ih / (2.0 * iw)).round() as u16;
    (cols, rows.clamp(1, 20))
}

/// Paint `frame` into `area` as half-block colour cells.
pub fn render(buf: &mut Buffer, area: Rect, frame: &Frame) {
    Video::new()
        .encoding(Encoding::HalfBlock)
        .render_frame(buf, area, frame);
}

#[cfg(test)]
mod tests {
    use super::*;
    use mullion::backend::TestBackend;
    use mullion::style::Color;
    use mullion::Terminal;

    /// A 32×32 JPEG of four saturated quadrants: TL red, TR green, BL blue, BR white.
    const JPG: &[u8] = include_bytes!("testdata/portrait.jpg");

    fn rgb(c: Color) -> (u8, u8, u8) {
        match c {
            Color::Rgb(r, g, b) => (r, g, b),
            other => panic!("expected Rgb, got {other:?}"),
        }
    }

    #[test]
    fn decodes_jpeg_to_frame_dimensions() {
        let f = decode(JPG).expect("jpeg should decode");
        assert_eq!((f.width(), f.height()), (32, 32));
        // Garbage in → None, never a panic.
        assert!(decode(b"not a jpeg").is_none());
    }

    #[test]
    fn portrait_height_follows_aspect() {
        // A square image at 24 cols → ~12 rows (cells are ~2× taller than wide).
        let f = decode(JPG).unwrap();
        assert_eq!(portrait_cells(&f), (24, 12));
    }

    #[test]
    fn renders_quadrant_colours_as_halfblocks() {
        let f = decode(JPG).unwrap();
        let area = Rect::new(0, 0, 8, 8);
        let mut term = Terminal::new(TestBackend::new(8, 8)).unwrap();
        term.draw(|buf| render(buf, area, &f)).unwrap();
        let buf = term.backend().buffer();

        // Cells are half-blocks carrying colour straight from the photo.
        assert_eq!(buf.get(1, 1).symbol, "▀");

        // Sample one cell well inside each quadrant; assert the dominant channel.
        let (r, g, b) = rgb(buf.get(1, 1).style.fg); // top-left → red
        assert!(r > g && r > b, "top-left should read red: {r},{g},{b}");
        let (r, g, b) = rgb(buf.get(6, 1).style.fg); // top-right → green
        assert!(g > r && g > b, "top-right should read green: {r},{g},{b}");
        let (r, g, b) = rgb(buf.get(1, 6).style.fg); // bottom-left → blue
        assert!(b > r && b > g, "bottom-left should read blue: {r},{g},{b}");
    }
}
