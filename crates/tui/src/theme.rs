use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier};

pub const PAPER: Color = Color::Rgb(0x14, 0x12, 0x0F);
pub const INK: Color = Color::Rgb(0xF0, 0xE9, 0xDC);
pub const TEXT: Color = Color::Rgb(0xBD, 0xB5, 0xA6);
pub const PENCIL: Color = Color::Rgb(0x83, 0x7C, 0x70);
pub const RULE: Color = Color::Rgb(0x3B, 0x37, 0x30);
pub const ACTION: Color = Color::Rgb(0xA6, 0xC7, 0x9A);
pub const ERROR: Color = Color::Rgb(0xE2, 0x80, 0x6E);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ColorMode {
    TrueColor,
    Ansi256,
    Ansi16,
    NoColor,
}

impl ColorMode {
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
}

pub fn degrade(buf: &mut Buffer, mode: ColorMode) {
    for cell in &mut buf.content {
        match mode {
            ColorMode::TrueColor => {}
            ColorMode::Ansi256 => {
                cell.fg = indexed(cell.fg);
                cell.bg = indexed(cell.bg);
            }
            ColorMode::Ansi16 => {
                cell.fg = named(cell.fg);
                cell.bg = Color::Reset;
            }
            ColorMode::NoColor => {
                if cell.fg == PENCIL || cell.fg == RULE {
                    cell.modifier.insert(Modifier::DIM);
                }
                cell.fg = Color::Reset;
                cell.bg = Color::Reset;
            }
        }
    }
}

fn indexed(c: Color) -> Color {
    let table: [(Color, u8); 7] =
        [(PAPER, 233), (INK, 255), (TEXT, 250), (PENCIL, 244), (RULE, 237), (ACTION, 151), (ERROR, 173)];
    match table.iter().find(|(token, _)| *token == c) {
        Some((_, index)) => Color::Indexed(*index),
        None => c,
    }
}

fn named(c: Color) -> Color {
    match c {
        _ if c == ACTION => Color::Green,
        _ if c == ERROR => Color::Red,
        _ if c == PENCIL || c == RULE => Color::DarkGray,
        _ => Color::Reset,
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

    fn degraded(fg: Color, mode: ColorMode) -> (Color, Color, bool) {
        let mut buf = Buffer::empty(ratatui::layout::Rect::new(0, 0, 1, 1));
        buf.content[0].fg = fg;
        buf.content[0].bg = PAPER;
        degrade(&mut buf, mode);
        let cell = &buf.content[0];
        (cell.fg, cell.bg, cell.modifier.contains(Modifier::DIM))
    }

    #[test]
    fn fewer_colors_keep_the_two_inks_and_the_pencil() {
        assert_eq!(degraded(ACTION, ColorMode::Ansi256), (Color::Indexed(151), Color::Indexed(233), false));
        assert_eq!(degraded(ACTION, ColorMode::Ansi16), (Color::Green, Color::Reset, false));
        assert_eq!(degraded(ERROR, ColorMode::Ansi16), (Color::Red, Color::Reset, false));
        assert_eq!(degraded(PENCIL, ColorMode::Ansi16), (Color::DarkGray, Color::Reset, false));
        assert_eq!(degraded(INK, ColorMode::Ansi16), (Color::Reset, Color::Reset, false));
        assert_eq!(degraded(PENCIL, ColorMode::NoColor), (Color::Reset, Color::Reset, true));
        assert_eq!(degraded(ERROR, ColorMode::NoColor), (Color::Reset, Color::Reset, false));
    }
}
