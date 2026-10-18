//! Vesper tokens (DESIGN-BRIEF 2.3). The view draws with these RGB values only, and
//! [`degrade`] rewrites the finished buffer for terminals with fewer colors (2.4).

use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier};

pub const BG: Color = Color::Rgb(0x10, 0x10, 0x10);
pub const PANEL: Color = Color::Rgb(0x16, 0x16, 0x16);
pub const ELEMENT: Color = Color::Rgb(0x1C, 0x1C, 0x1C);
pub const SELECTION: Color = Color::Rgb(0x23, 0x23, 0x23);
pub const BORDER: Color = Color::Rgb(0x28, 0x28, 0x28);
/// The background of empty progress cells. Same value as [`BORDER`].
pub const TRACK: Color = BORDER;
pub const TEXT: Color = Color::Rgb(0xFF, 0xFF, 0xFF);
pub const MUTED: Color = Color::Rgb(0xA0, 0xA0, 0xA0);
pub const DIM: Color = Color::Rgb(0x7E, 0x7E, 0x7E);
pub const FAINT: Color = Color::Rgb(0x50, 0x50, 0x50);
pub const ACCENT: Color = Color::Rgb(0xFF, 0xC7, 0x99);
pub const ON_ACCENT: Color = Color::Rgb(0x00, 0x00, 0x00);
/// `accent-2`: positive provenance flags.
pub const MINT: Color = Color::Rgb(0x99, 0xFF, 0xE4);
pub const SUCCESS: Color = MINT;
pub const WARNING: Color = Color::Rgb(0xFF, 0x73, 0x00);
pub const ERROR: Color = Color::Rgb(0xFF, 0x80, 0x80);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ColorMode {
    TrueColor,
    Ansi256,
    Ansi16,
    NoColor,
}

impl ColorMode {
    /// `NO_COLOR`, then `COLORTERM`, then `TERM`, in the order section 2.4 gives.
    pub fn detect(var: impl Fn(&str) -> Option<String>) -> Self {
        let set = |key| var(key).filter(|v| !v.is_empty());
        if set("NO_COLOR").is_some() {
            return Self::NoColor;
        }
        if set("COLORTERM").is_some_and(|v| v == "truecolor" || v == "24bit") {
            return Self::TrueColor;
        }
        match set("TERM") {
            Some(term) if term.contains("256color") => Self::Ansi256,
            Some(term) if term == "dumb" => Self::NoColor,
            _ => Self::Ansi16,
        }
    }

    /// Whether backgrounds are painted, and so whether dialogs dim the screen behind them.
    pub fn paints_backgrounds(self) -> bool {
        matches!(self, Self::TrueColor | Self::Ansi256)
    }
}

/// Multiplies every color by 0.412, which matches a black overlay at alpha 150 (section 3.11).
pub fn dim(buf: &mut Buffer) {
    let fade = |c: Color| match c {
        Color::Rgb(r, g, b) => {
            let f = |v: u8| (f32::from(v) * 0.412).round() as u8;
            Color::Rgb(f(r), f(g), f(b))
        }
        other => other,
    };
    for cell in &mut buf.content {
        cell.fg = fade(cell.fg);
        cell.bg = fade(cell.bg);
    }
}

/// Rewrites a buffer drawn with the Vesper tokens for a terminal with fewer colors.
pub fn degrade(buf: &mut Buffer, mode: ColorMode) {
    match mode {
        ColorMode::TrueColor => {}
        ColorMode::Ansi256 => {
            for cell in &mut buf.content {
                cell.fg = indexed(cell.fg);
                cell.bg = indexed(cell.bg);
            }
        }
        ColorMode::Ansi16 | ColorMode::NoColor => {
            for cell in &mut buf.content {
                let filled = cell.bg == SELECTION || cell.bg == ACCENT;
                if cell.bg == TRACK && cell.symbol() == " " {
                    cell.set_symbol("░");
                    cell.fg = FAINT;
                }
                cell.fg = if mode == ColorMode::NoColor { Color::Reset } else { named(cell.fg) };
                cell.bg = Color::Reset;
                if filled {
                    cell.modifier.insert(Modifier::REVERSED);
                }
            }
        }
    }
}

/// The xterm index for each token (section 2.4), and the nearest one for dimmed colors.
fn indexed(c: Color) -> Color {
    let table: [(Color, u8); 13] = [
        (BG, 233),
        (PANEL, 234),
        (ELEMENT, 235),
        (SELECTION, 236),
        (BORDER, 236),
        (TEXT, 231),
        (MUTED, 247),
        (DIM, 244),
        (FAINT, 239),
        (ACCENT, 222),
        (ON_ACCENT, 16),
        (MINT, 122),
        (WARNING, 202),
    ];
    match c {
        Color::Rgb(r, g, b) => match table.iter().find(|(token, _)| *token == c) {
            Some((_, index)) => Color::Indexed(*index),
            None if c == ERROR => Color::Indexed(210),
            None => Color::Indexed(nearest_xterm(r, g, b)),
        },
        other => other,
    }
}

fn nearest_xterm(r: u8, g: u8, b: u8) -> u8 {
    let level = |v: u8| {
        if v < 48 {
            0
        } else if v < 115 {
            1
        } else {
            (v - 35) / 40
        }
    };
    let steps = [0, 95, 135, 175, 215, 255];
    let (cr, cg, cb) = (level(r), level(g), level(b));
    let cube = 16 + 36 * cr + 6 * cg + cb;
    let grey_level = ((u16::from(r) + u16::from(g) + u16::from(b)) / 3) as u8;
    let grey = if grey_level > 238 { 255 } else { 232 + grey_level.saturating_sub(3) / 10 };
    let distance = |(x, y, z): (u8, u8, u8)| {
        let d = |p: u8, q: u8| (i32::from(p) - i32::from(q)).pow(2);
        d(x, r) + d(y, g) + d(z, b)
    };
    let cube_rgb = (steps[cr as usize], steps[cg as usize], steps[cb as usize]);
    let grey_value = if grey == 255 { 255 } else { 8 + (grey - 232) * 10 };
    if grey != 255 && distance((grey_value, grey_value, grey_value)) < distance(cube_rgb) { grey } else { cube }
}

fn named(c: Color) -> Color {
    match c {
        _ if c == ACCENT || c == WARNING => Color::Yellow,
        _ if c == MINT => Color::Green,
        _ if c == ERROR => Color::Red,
        _ if c == ON_ACCENT || c == TEXT => Color::Reset,
        Color::Rgb(..) => Color::DarkGray,
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_the_color_mode_from_the_environment() {
        let detect = |pairs: &[(&str, &str)]| {
            ColorMode::detect(|k| pairs.iter().find(|(key, _)| *key == k).map(|(_, v)| v.to_string()))
        };
        assert_eq!(detect(&[("COLORTERM", "truecolor"), ("TERM", "xterm-256color")]), ColorMode::TrueColor);
        assert_eq!(detect(&[("TERM", "xterm-256color")]), ColorMode::Ansi256);
        assert_eq!(detect(&[("TERM", "xterm")]), ColorMode::Ansi16);
        assert_eq!(detect(&[("TERM", "dumb")]), ColorMode::NoColor);
        assert_eq!(detect(&[("NO_COLOR", "1"), ("COLORTERM", "truecolor")]), ColorMode::NoColor);
        assert_eq!(detect(&[("NO_COLOR", ""), ("COLORTERM", "24bit")]), ColorMode::TrueColor);
    }

    #[test]
    fn dimming_matches_the_brief() {
        let mut buf = Buffer::empty(ratatui::layout::Rect::new(0, 0, 2, 1));
        buf.content[0].fg = ACCENT;
        buf.content[0].bg = BG;
        buf.content[1].fg = MINT;
        dim(&mut buf);
        assert_eq!(buf.content[0].fg, Color::Rgb(0x69, 0x52, 0x3F));
        assert_eq!(buf.content[0].bg, Color::Rgb(0x07, 0x07, 0x07));
        assert_eq!(buf.content[1].fg, Color::Rgb(0x3F, 0x69, 0x5E));
    }

    #[test]
    fn sixteen_colors_reverse_the_selection_and_draw_empty_bar_cells() {
        let mut buf = Buffer::empty(ratatui::layout::Rect::new(0, 0, 2, 1));
        buf.content[0].bg = SELECTION;
        buf.content[0].fg = ACCENT;
        buf.content[1].bg = TRACK;
        degrade(&mut buf, ColorMode::Ansi16);
        assert_eq!((buf.content[0].fg, buf.content[0].bg), (Color::Yellow, Color::Reset));
        assert!(buf.content[0].modifier.contains(Modifier::REVERSED));
        assert_eq!((buf.content[1].symbol(), buf.content[1].fg), ("░", Color::DarkGray));
    }

    #[test]
    fn the_256_color_table_keeps_the_layers_apart() {
        assert_eq!(
            (indexed(BG), indexed(PANEL), indexed(ELEMENT)),
            (Color::Indexed(233), Color::Indexed(234), Color::Indexed(235))
        );
        assert_eq!(indexed(ERROR), Color::Indexed(210));
        assert_eq!(
            indexed(Color::Rgb(0x69, 0x52, 0x3F)),
            Color::Indexed(240),
            "a dimmed accent takes the nearest grey"
        );
    }
}
