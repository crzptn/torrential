//! Dates as sources write them, turned into Unix seconds.

use time::format_description::well_known::{Rfc2822, Rfc3339};
use time::{Date, Month, OffsetDateTime, PrimitiveDateTime, Time};

/// `2026-06-03T21:09:06+00:00` or `2021-02-03T16:09:29Z`.
pub(crate) fn rfc3339(s: &str) -> Option<i64> {
    OffsetDateTime::parse(s.trim(), &Rfc3339).ok().map(OffsetDateTime::unix_timestamp)
}

/// `Thu, 15 Oct 2026 14:00:32 -0000`, as RSS writes it.
pub(crate) fn rfc2822(s: &str) -> Option<i64> {
    OffsetDateTime::parse(s.trim(), &Rfc2822).ok().map(OffsetDateTime::unix_timestamp)
}

/// `2026-10-01`, or `2026-09-12 13:32` as directory listings write it, read as UTC.
pub(crate) fn civil(s: &str) -> Option<i64> {
    let (day, clock) = s.trim().split_once(' ').unwrap_or((s.trim(), "00:00"));
    let mut ymd = day.splitn(3, '-').map(str::parse::<u16>);
    let (y, m, d) = (ymd.next()?.ok()?, ymd.next()?.ok()?, ymd.next()?.ok()?);
    let (h, min) = clock.split_once(':')?;
    let date =
        Date::from_calendar_date(i32::from(y), Month::try_from(u8::try_from(m).ok()?).ok()?, u8::try_from(d).ok()?)
            .ok()?;
    let time = Time::from_hms(h.parse().ok()?, min.parse().ok()?, 0).ok()?;
    Some(PrimitiveDateTime::new(date, time).assume_utc().unix_timestamp())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_each_source_format() {
        assert_eq!(rfc3339("2026-06-03T21:09:06+00:00"), Some(1_780_520_946));
        assert_eq!(rfc3339("2021-02-03T16:09:29Z"), Some(1_612_368_569));
        assert_eq!(rfc2822("Thu, 15 Oct 2026 14:00:32 -0000"), Some(1_792_072_832));
        assert_eq!(civil("2026-10-01"), Some(1_790_812_800));
        assert_eq!(civil("2026-09-12 13:32"), Some(1_789_219_920));
        assert_eq!(civil("yesterday"), None);
    }
}
