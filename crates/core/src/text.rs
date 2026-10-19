/// Removes terminal escape sequences, control characters, and bidirectional overrides,
/// and folds every run of whitespace, tabs and newlines included, into one space.
pub fn sanitize(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    let mut pending_space = false;
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            skip_escape(&mut chars);
            continue;
        }
        if c.is_whitespace() {
            pending_space = !out.is_empty();
            continue;
        }
        if c.is_control() || is_invisible_format(c) {
            continue;
        }
        if pending_space {
            out.push(' ');
            pending_space = false;
        }
        out.push(c);
    }
    out
}

/// Cuts `input` to at most `max` characters, ending with an ellipsis when cut.
pub fn truncate(input: &str, max: usize) -> String {
    if input.chars().count() <= max {
        return input.to_string();
    }
    let Some(keep) = max.checked_sub(1) else {
        return String::new();
    };
    let mut out: String = input.chars().take(keep).collect();
    out.push('…');
    out
}

fn is_invisible_format(c: char) -> bool {
    matches!(c, '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{2069}' | '\u{feff}')
}

fn skip_escape(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    match chars.next() {
        // CSI: parameters and intermediates, then one final byte in @..~.
        Some('[') => {
            for c in chars.by_ref() {
                if ('@'..='~').contains(&c) {
                    break;
                }
            }
        }
        // OSC, DCS, and similar strings end at BEL or ESC \.
        Some(']' | 'P' | '^' | '_' | 'X') => {
            while let Some(c) = chars.next() {
                if c == '\u{7}' {
                    break;
                }
                if c == '\u{1b}' && chars.peek() == Some(&'\\') {
                    chars.next();
                    break;
                }
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_escapes_controls_and_bidi_overrides() {
        let hostile = "Ubuntu\u{1b}[31m 24.04\u{1b}]52;c;cm0gLXJm\u{7}\t ISO\u{202e}gpj.exe\u{0}";
        assert_eq!(sanitize(hostile), "Ubuntu 24.04 ISOgpj.exe");
    }

    #[test]
    fn folds_whitespace_and_keeps_unicode_letters() {
        assert_eq!(sanitize("  Сериал\n\n2024   1080p  "), "Сериал 2024 1080p");
    }

    #[test]
    fn truncates_with_an_ellipsis() {
        assert_eq!(truncate("archlinux-2026.10.01", 9), "archlinu…");
        assert_eq!(truncate("short", 9), "short");
        assert_eq!(truncate("short", 1), "…");
        assert_eq!(truncate("short", 0), "");
    }
}
