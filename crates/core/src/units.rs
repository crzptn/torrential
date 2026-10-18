//! Sizes, durations, and counts as people type and read them.

use std::time::Duration;

/// Parses `8GiB`, `1.5 GB`, `700MB`, or a plain byte count. IEC units (KiB, MiB, GiB,
/// TiB) are powers of 1024. SI units (KB, MB, GB, TB) are powers of 1000.
pub fn parse_size(input: &str) -> Option<u64> {
    let s = input.trim();
    let split = s.find(|c: char| !(c.is_ascii_digit() || c == '.')).unwrap_or(s.len());
    let (number, unit) = s.split_at(split);
    let number: f64 = number.parse().ok()?;
    let multiplier: f64 = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1.0,
        "kb" | "k" => 1e3,
        "mb" | "m" => 1e6,
        "gb" | "g" => 1e9,
        "tb" | "t" => 1e12,
        "kib" => 1024.0,
        "mib" => 1024.0 * 1024.0,
        "gib" => 1024.0 * 1024.0 * 1024.0,
        "tib" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    let bytes = number * multiplier;
    (bytes.is_finite() && bytes >= 0.0 && bytes <= u64::MAX as f64).then(|| bytes.round() as u64)
}

/// Formats bytes with IEC units and one decimal: `1.5 GiB`, `568.8 MiB`, `912 B`.
pub fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

/// A limit as people type it: `8 GB` or `50 GiB` when it is a whole number of one unit,
/// and otherwise like [`format_size`].
pub fn format_limit(bytes: u64) -> String {
    whole_units(bytes).map_or_else(|| format_size(bytes), |(n, unit)| format!("{n} {unit}"))
}

/// A size that [`parse_size`] reads back exactly: `8GB`, `50GiB`, or a byte count.
pub fn size_text(bytes: u64) -> String {
    whole_units(bytes).map_or_else(|| bytes.to_string(), |(n, unit)| format!("{n}{unit}"))
}

fn whole_units(bytes: u64) -> Option<(u64, &'static str)> {
    const UNITS: [(&str, u64); 6] = [
        ("TiB", 1 << 40),
        ("TB", 1_000_000_000_000),
        ("GiB", 1 << 30),
        ("GB", 1_000_000_000),
        ("MiB", 1 << 20),
        ("MB", 1_000_000),
    ];
    UNITS
        .iter()
        .find(|(_, unit)| bytes >= *unit && bytes.is_multiple_of(*unit))
        .map(|(name, unit)| (bytes / unit, *name))
}

pub fn format_rate(bytes_per_sec: u64) -> String {
    format!("{}/s", format_size(bytes_per_sec))
}

/// Parses `100s`, `90`, `5m`, `2h`, `1h30m`, or `1h 30m 15s`. A bare number is seconds.
pub fn parse_duration(input: &str) -> Option<Duration> {
    let s: String = input.chars().filter(|c| !c.is_whitespace()).collect();
    if s.is_empty() {
        return None;
    }
    if let Ok(secs) = s.parse::<u64>() {
        return Some(Duration::from_secs(secs));
    }
    let mut total = 0u64;
    let mut digits = String::new();
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c.is_ascii_digit() {
            digits.push(c);
            continue;
        }
        let unit = if c == 'm' && chars.peek() == Some(&'s') {
            chars.next();
            "ms"
        } else {
            match c {
                'h' => "h",
                'm' => "m",
                's' => "s",
                'd' => "d",
                _ => return None,
            }
        };
        let n: u64 = digits.parse().ok()?;
        digits.clear();
        total = total.checked_add(match unit {
            "ms" => return (total == 0 && chars.peek().is_none()).then(|| Duration::from_millis(n)),
            "s" => n,
            "m" => n.checked_mul(60)?,
            "h" => n.checked_mul(3600)?,
            _ => n.checked_mul(86_400)?,
        })?;
    }
    digits.is_empty().then(|| Duration::from_secs(total))
}

/// Formats a duration compactly: `48s`, `16m`, `2h11m`, `3d4h`.
pub fn format_duration(d: Duration) -> String {
    let secs = d.as_secs();
    match secs {
        0..=59 => format!("{secs}s"),
        60..=3599 => format!("{}m", secs / 60),
        3600..=86_399 => {
            let (h, m) = (secs / 3600, secs % 3600 / 60);
            if m == 0 { format!("{h}h") } else { format!("{h}h{m:02}m") }
        }
        _ => {
            let (days, h) = (secs / 86_400, secs % 86_400 / 3600);
            if h == 0 { format!("{days}d") } else { format!("{days}d{h}h") }
        }
    }
}

/// `1 file`, `2 files`. Every noun counted in output is regular.
pub fn count(n: usize, noun: &str) -> String {
    if n == 1 { format!("1 {noun}") } else { format!("{n} {noun}s") }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_iec_and_si_sizes() {
        assert_eq!(parse_size("8GiB"), Some(8 * 1024 * 1024 * 1024));
        assert_eq!(parse_size("1.5 GB"), Some(1_500_000_000));
        assert_eq!(parse_size("700mb"), Some(700_000_000));
        assert_eq!(parse_size("4096"), Some(4096));
        assert_eq!(parse_size("12 parsecs"), None);
    }

    #[test]
    fn formats_sizes_and_rates() {
        assert_eq!(format_size(912), "912 B");
        assert_eq!(format_size(1_640_497_152), "1.5 GiB");
        assert_eq!(format_size(596_399_542), "568.8 MiB");
        assert_eq!(format_rate(40_265_318), "38.4 MiB/s");
    }

    #[test]
    fn limits_read_as_typed_and_round_trip() {
        let sizes = [8_000_000_000, 50 << 30, 1_500_000_000, 1_234_567];
        assert_eq!(sizes.map(format_limit), ["8 GB", "50 GiB", "1500 MB", "1.2 MiB"]);
        assert_eq!(sizes.map(size_text), ["8GB", "50GiB", "1500MB", "1234567"]);
        assert_eq!(sizes.map(|s| parse_size(&size_text(s))), sizes.map(Some));
    }

    #[test]
    fn parses_durations() {
        assert_eq!(parse_duration("100s"), Some(Duration::from_secs(100)));
        assert_eq!(parse_duration("90"), Some(Duration::from_secs(90)));
        assert_eq!(parse_duration("110m"), Some(Duration::from_secs(6600)));
        assert_eq!(parse_duration("1h 30m 15s"), Some(Duration::from_secs(5415)));
        assert_eq!(parse_duration("250ms"), Some(Duration::from_millis(250)));
        assert_eq!(parse_duration("5x"), None);
        assert_eq!(parse_duration("5h3"), None);
    }

    #[test]
    fn formats_durations_compactly() {
        assert_eq!(format_duration(Duration::from_secs(48)), "48s");
        assert_eq!(format_duration(Duration::from_secs(16 * 60 + 5)), "16m");
        assert_eq!(format_duration(Duration::from_secs(2 * 3600 + 11 * 60)), "2h11m");
        assert_eq!(format_duration(Duration::from_secs(3 * 86_400 + 4 * 3600)), "3d4h");
    }

    #[test]
    fn counts_agree_with_their_number() {
        assert_eq!(count(0, "file"), "0 files");
        assert_eq!(count(1, "file"), "1 file");
        assert_eq!(count(2, "damaged piece"), "2 damaged pieces");
    }
}
