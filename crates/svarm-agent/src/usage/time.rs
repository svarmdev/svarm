//! Timestamp reading for usage probes.
//!
//! Providers disagree about how they express a reset: Codex sends unix seconds, Claude sends
//! RFC 3339 text. Both are accepted here and anything else yields `None`, so an unrecognised
//! format surfaces as "reset time not reported" rather than as a plausible wrong time.

use serde::Deserialize;

/// A reset instant as a provider expressed it.
#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(untagged)]
pub(crate) enum Timestamp {
    Unix(i64),
    Text(String),
}

impl Timestamp {
    /// Milliseconds since the unix epoch, or `None` when the value cannot be read exactly.
    pub(crate) fn to_unix_ms(&self) -> Option<u64> {
        match self {
            Self::Unix(seconds) => u64::try_from(*seconds).ok()?.checked_mul(1000),
            Self::Text(text) => parse_rfc3339_ms(text),
        }
    }
}

/// Parse the RFC 3339 subset providers actually emit: `2026-08-19T22:00:00.366440+00:00`,
/// with an optional fractional part and either `Z` or a numeric offset.
pub(crate) fn parse_rfc3339_ms(text: &str) -> Option<u64> {
    let text = text.trim();
    let bytes = text.as_bytes();
    if bytes.len() < 19 || bytes[4] != b'-' || bytes[7] != b'-' {
        return None;
    }
    if !matches!(bytes[10], b'T' | b't' | b' ') || bytes[13] != b':' || bytes[16] != b':' {
        return None;
    }

    let year: i64 = text.get(0..4)?.parse().ok()?;
    let month: u32 = text.get(5..7)?.parse().ok()?;
    let day: u32 = text.get(8..10)?.parse().ok()?;
    let hour: i64 = text.get(11..13)?.parse().ok()?;
    let minute: i64 = text.get(14..16)?.parse().ok()?;
    let second: i64 = text.get(17..19)?.parse().ok()?;

    if !(1..=12).contains(&month) || day < 1 || day > days_in_month(year, month) {
        return None;
    }
    if hour > 23 || minute > 59 || second > 60 {
        return None;
    }

    // Skip the fractional part; sub-second precision never changes a displayed countdown.
    let mut rest = &text[19..];
    if let Some(fraction) = rest.strip_prefix('.') {
        let digits = fraction.bytes().take_while(u8::is_ascii_digit).count();
        if digits == 0 {
            return None;
        }
        rest = &fraction[digits..];
    }

    let offset_minutes = parse_offset_minutes(rest)?;
    let days = days_from_civil(year, month, day);
    let seconds = days
        .checked_mul(86_400)?
        .checked_add(hour * 3600 + minute * 60 + second)?
        .checked_sub(offset_minutes * 60)?;
    u64::try_from(seconds).ok()?.checked_mul(1000)
}

/// `Z`, `+HH:MM`, `-HHMM`, or an empty tail (treated as UTC, which is what these APIs send).
fn parse_offset_minutes(text: &str) -> Option<i64> {
    if text.is_empty() || text.eq_ignore_ascii_case("z") {
        return Some(0);
    }
    let (sign, rest) = match text.as_bytes().first()? {
        b'+' => (1, &text[1..]),
        b'-' => (-1, &text[1..]),
        _ => return None,
    };
    let digits: String = rest.chars().filter(|c| *c != ':').collect();
    if digits.len() != 4 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let hours: i64 = digits.get(0..2)?.parse().ok()?;
    let minutes: i64 = digits.get(2..4)?.parse().ok()?;
    if hours > 23 || minutes > 59 {
        return None;
    }
    Some(sign * (hours * 60 + minutes))
}

const fn is_leap_year(year: i64) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

const fn days_in_month(year: i64, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

/// Days since 1970-01-01, by Howard Hinnant's `days_from_civil`.
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let year_of_era = year - era * 400;
    let month = i64::from(month);
    let day_of_year =
        (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unix_seconds_and_rfc3339_agree_on_the_same_instant() {
        // 2026-08-19T22:00:00Z, cross-checked against `date -u -d ... +%s`.
        assert_eq!(
            parse_rfc3339_ms("2026-08-19T22:00:00+00:00"),
            Timestamp::Unix(1_787_176_800).to_unix_ms()
        );
        assert_eq!(
            parse_rfc3339_ms("2026-08-19T22:00:00Z"),
            parse_rfc3339_ms("2026-08-19T22:00:00.366440+00:00")
        );
    }

    #[test]
    fn the_real_claude_and_codex_values_parse() {
        // Captured live from the two providers.
        assert!(parse_rfc3339_ms("2026-08-15T00:00:00.366387+00:00").is_some());
        assert_eq!(
            Timestamp::Unix(1_787_201_323).to_unix_ms(),
            Some(1_787_201_323_000)
        );
    }

    #[test]
    fn offsets_shift_the_instant() {
        let utc = parse_rfc3339_ms("2026-08-19T22:00:00Z").unwrap();
        assert_eq!(parse_rfc3339_ms("2026-08-20T00:00:00+02:00"), Some(utc));
        assert_eq!(parse_rfc3339_ms("2026-08-19T20:00:00-0200"), Some(utc));
    }

    #[test]
    fn leap_days_and_month_boundaries_are_exact() {
        let epoch = parse_rfc3339_ms("1970-01-01T00:00:00Z");
        assert_eq!(epoch, Some(0));
        assert_eq!(
            parse_rfc3339_ms("2024-02-29T00:00:00Z"),
            Some(1_709_164_800_000)
        );
        // 2100 is not a leap year, so 29 February does not exist.
        assert_eq!(parse_rfc3339_ms("2100-02-29T00:00:00Z"), None);
        assert_eq!(parse_rfc3339_ms("2024-02-30T00:00:00Z"), None);
    }

    #[test]
    fn unreadable_timestamps_report_nothing_rather_than_guessing() {
        for text in [
            "",
            "not a time",
            "2026-08-19",
            "2026-13-01T00:00:00Z",
            "2026-08-19T24:00:00Z",
            "2026-08-19T22:00:00+99:00",
            "2026-08-19T22:00:00.Z",
            "2026-08-19T22:00:00 CEST",
        ] {
            assert_eq!(
                parse_rfc3339_ms(text),
                None,
                "expected {text:?} to be unreadable"
            );
        }
        assert_eq!(Timestamp::Text("nonsense".into()).to_unix_ms(), None);
        // A negative unix time predates the epoch and is never a subscription reset.
        assert_eq!(Timestamp::Unix(-1).to_unix_ms(), None);
    }
}
