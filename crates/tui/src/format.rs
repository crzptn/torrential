//! Text as the screen shows it: widths in cells, truncation, and short number forms that line
//! up in columns.

use std::path::Path;

use ratatui::text::Span;

pub fn width(s: &str) -> usize {
    Span::raw(s).width()
}

fn char_width(c: char) -> usize {
    width(c.encode_utf8(&mut [0; 4]))
}

/// Cuts `s` to `max` cells, ending with `…` when cut.
pub fn ellipsize(s: &str, max: usize) -> String {
    if width(s) <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let w = char_width(c);
        if used + w > max - 1 {
            break;
        }
        out.push(c);
        used += w;
    }
    out.push('…');
    out
}

/// Cuts `s` in the middle, keeping its start and end (opencode's `truncateMiddle`).
pub fn middle(s: &str, max: usize) -> String {
    if width(s) <= max || max < 5 {
        return ellipsize(s, max);
    }
    let head = (max - 1).div_ceil(2);
    let tail = (max - 1) / 2;
    let mut start = String::new();
    let mut used = 0;
    for c in s.chars() {
        if used + char_width(c) > head {
            break;
        }
        used += char_width(c);
        start.push(c);
    }
    let mut end: Vec<char> = Vec::new();
    let mut used = 0;
    for c in s.chars().rev() {
        if used + char_width(c) > tail {
            break;
        }
        used += char_width(c);
        end.push(c);
    }
    start.push('…');
    start.extend(end.into_iter().rev());
    start
}

/// `now` under a minute, then `45m`, `5h`, `17d`, `2mo`, `3y`.
pub fn age(published: i64, now: i64) -> String {
    let secs = (now - published).max(0);
    let (minute, hour, day) = (60, 3_600, 86_400);
    match secs {
        s if s < minute => "now".into(),
        s if s < hour => format!("{}m", s / minute),
        s if s < day => format!("{}h", s / hour),
        s if s < 60 * day => format!("{}d", s / day),
        s if s < 365 * day => format!("{}mo", s / (30 * day)),
        s => format!("{}y", s / (365 * day)),
    }
}

/// Three figures at most, so sizes fit an 8-cell column: `512 B`, `51.4 KiB`, `734 MiB`,
/// `1.3 GiB`. A value that would print as 1000 or more moves up a unit.
pub fn size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value.round() >= 1000.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    match (unit, value) {
        (0, v) => format!("{v} B"),
        (_, v) if v < 99.95 => format!("{v:.1} {}", UNITS[unit]),
        (_, v) => format!("{v:.0} {}", UNITS[unit]),
    }
}

pub fn rate(bytes_per_sec: u64) -> String {
    format!("{}/s", size(bytes_per_sec))
}

/// `0` to `9,999` with a comma, then `10k` to `999k`, and `–` when unknown.
pub fn seeds(n: Option<u32>) -> String {
    match n {
        None => "–".into(),
        Some(n) if n < 1_000 => n.to_string(),
        Some(n) if n < 10_000 => format!("{},{:03}", n / 1_000, n % 1_000),
        Some(n) => format!("{}k", (n + 500) / 1_000),
    }
}

/// A path with the home folder written as `~`.
pub fn home_path(path: &Path, home: Option<&Path>) -> String {
    match home.and_then(|h| path.strip_prefix(h).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".into(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

/// `62%`, rounded down so a full bar always means done.
pub fn percent(done: u64, size: u64) -> String {
    if size == 0 {
        return "100%".into();
    }
    format!("{}%", (u128::from(done) * 100 / u128::from(size)) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ellipsizes_by_display_width() {
        assert_eq!(ellipsize("archlinux-2026.10.01", 9), "archlinu…");
        assert_eq!(ellipsize("short", 9), "short");
        assert_eq!(ellipsize("日本語のタイトル", 7), "日本語…");
    }

    #[test]
    fn cuts_paths_in_the_middle() {
        assert_eq!(middle("~/Downloads/night_of_the_living_dead_meta.xml", 30), "~/Downloads/nig…_dead_meta.xml");
        assert_eq!(middle("~/x", 30), "~/x");
    }

    #[test]
    fn ages_use_the_largest_whole_unit() {
        let now = 1_000_000_000;
        let ago = |secs: i64| age(now - secs, now);
        assert_eq!(
            [ago(30), ago(45 * 60), ago(5 * 3_600), ago(17 * 86_400), ago(70 * 86_400), ago(9 * 365 * 86_400)],
            ["now", "45m", "5h", "17d", "2mo", "9y"]
        );
    }

    #[test]
    fn sizes_keep_three_figures() {
        let mib = 1 << 20;
        assert_eq!(
            [
                size(512),
                size(52_633),
                size(734 * mib),
                size(1022 * mib + mib / 2),
                size(1366 * mib),
                size(21_812 * mib)
            ],
            ["512 B", "51.4 KiB", "734 MiB", "1.0 GiB", "1.3 GiB", "21.3 GiB"]
        );
        assert_eq!(rate(10 * mib), "10.0 MiB/s");
    }

    #[test]
    fn seeds_read_short() {
        assert_eq!([seeds(None), seeds(Some(0)), seeds(Some(2_041)), seeds(Some(41_200))], ["–", "0", "2,041", "41k"]);
    }

    #[test]
    fn home_becomes_a_tilde() {
        let home = Path::new("/Users/ada");
        assert_eq!(home_path(Path::new("/Users/ada/Downloads/x"), Some(home)), "~/Downloads/x");
        assert_eq!(home_path(Path::new("/srv/x"), Some(home)), "/srv/x");
    }

    #[test]
    fn percent_rounds_down() {
        assert_eq!([percent(999, 1000), percent(1000, 1000), percent(0, 0)], ["99%", "100%", "100%"]);
    }
}
