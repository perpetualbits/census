//! Colour palette and `Style` helpers shared across every screen and overlay.

use mullion::{
    border::{BorderStyle, CornerStyle, LineWeight},
    style::{Color, Modifier, Style},
};

// ─── palette ─────────────────────────────────────────────────────────────────

pub const C_BORDER: Color = Color::Rgb(70,  70,  100);
pub const C_FG:     Color = Color::Rgb(200, 200, 210);
pub const C_DIM:    Color = Color::Rgb(110, 110, 130);
pub const C_HEAD:   Color = Color::Rgb(255, 255, 255);
pub const C_HDR2:   Color = Color::Rgb(140, 170, 255);
pub const C_TITLE:  Color = Color::Rgb(160, 160, 255);
pub const C_SEL_FG: Color = Color::Rgb(0,   0,   0  );
pub const C_SEL_BG: Color = Color::Rgb(80,  120, 210);
pub const C_MEMBER: Color = Color::Rgb(80,  190, 100);
pub const C_OK:     Color = Color::Rgb(80,  200, 100);
pub const C_ERR:    Color = Color::Rgb(220, 80,  80 );

// ─── style helpers ───────────────────────────────────────────────────────────

pub fn s_border()  -> Style { Style::default().fg(C_BORDER) }
pub fn s_normal()  -> Style { Style::default().fg(C_FG) }
pub fn s_dim()     -> Style { Style::default().fg(C_DIM) }
pub fn s_title()   -> Style { Style::default().fg(C_TITLE) }
pub fn s_head()    -> Style { Style::default().fg(C_HEAD).add_modifier(Modifier::BOLD) }
pub fn s_subhead() -> Style { Style::default().fg(C_HDR2) }
pub fn s_sel()     -> Style { Style::default().fg(C_SEL_FG).bg(C_SEL_BG) }
pub fn s_member()  -> Style { Style::default().fg(C_MEMBER) }
pub fn s_ok()      -> Style { Style::default().fg(C_OK) }
pub fn s_err()     -> Style { Style::default().fg(C_ERR) }

pub fn box_style() -> BorderStyle {
    BorderStyle { weight: LineWeight::Light, corners: CornerStyle::Rounded, style: s_border() }
}
