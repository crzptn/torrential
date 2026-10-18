//! Vesper's background (#101010) and text (#FFFFFF) are left to the terminal, because
//! forcing them breaks light themes.

use anstyle::{Color, RgbColor, Style};

pub const MUTED: RgbColor = RgbColor(0xA0, 0xA0, 0xA0);
pub const ACCENT: RgbColor = RgbColor(0xFF, 0xC7, 0x99);
pub const MINT: RgbColor = RgbColor(0x99, 0xFF, 0xE4);
pub const ERROR: RgbColor = RgbColor(0xFF, 0x80, 0x80);

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Token {
    #[default]
    Text,
    Muted,
    Accent,
    Mint,
    Error,
    Heading,
}

impl Token {
    pub fn style(self) -> Style {
        let fg = |c: RgbColor| Style::new().fg_color(Some(Color::Rgb(c)));
        match self {
            Self::Text => Style::new(),
            Self::Muted => fg(MUTED),
            Self::Accent => fg(ACCENT),
            Self::Mint => fg(MINT),
            Self::Error => fg(ERROR),
            Self::Heading => fg(MUTED).bold(),
        }
    }

    /// The indicatif template color for progress bars, which take named or 256 colors only.
    pub fn bar_color(self) -> &'static str {
        match self {
            Self::Mint => "123",
            Self::Accent => "223",
            Self::Error => "210",
            Self::Muted | Self::Heading => "248",
            Self::Text => "white",
        }
    }
}

pub fn paint(token: Token, text: &str) -> String {
    let style = token.style();
    if style == Style::new() { text.to_string() } else { format!("{style}{text}{style:#}") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paint_wraps_text_in_truecolor_escapes() {
        assert_eq!(paint(Token::Mint, "done"), "\u{1b}[38;2;153;255;228mdone\u{1b}[0m");
        assert_eq!(paint(Token::Text, "plain"), "plain");
    }
}
