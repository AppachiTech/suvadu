use chrono::{
    DateTime, Local, LocalResult, NaiveDate, NaiveDateTime, NaiveTime, TimeDelta, TimeZone,
};

// ── Timestamps ─────────────────────────────────────────

/// Threshold above which a timestamp is treated as microseconds (not milliseconds).
/// `9_999_999_999_999` is ~Nov 2286 in milliseconds, so any value above it is certainly
/// microseconds (16+ digits). Used consistently across all display and normalization code.
pub const MICROSECOND_THRESHOLD: i64 = 9_999_999_999_999;

/// Normalize a timestamp for display by converting microseconds to milliseconds.
/// This is the single function all display/formatting code should call.
/// Does NOT handle seconds→ms conversion (that's `normalize_timestamp` for ingestion).
pub const fn normalize_display_ms(ts: i64) -> i64 {
    if ts > MICROSECOND_THRESHOLD {
        ts / 1000
    } else {
        ts
    }
}

/// The date and time formats [`parse_date_input`] accepts, in the words an
/// error message or a tool description should use.
pub const DATE_INPUT_FORMATS: &str =
    "today, yesterday, N minutes ago, N hours ago, N days ago, N weeks ago, \
     YYYY-MM-DD, or an ISO 8601 date-time such as 2026-10-18T10:00:00Z or \
     2026-10-18T10:00:00+05:30 (no offset means local time)";

/// Parse a date string input into a Unix timestamp (milliseconds).
///
/// Supported formats:
/// - "YYYY-MM-DD" -> that local day, at the start or end (see below)
/// - "today" / "yesterday" -> that local day, at the start or end
/// - "N days ago" / "N weeks ago" -> that local day, at the start or end
/// - "N minutes ago" / "N hours ago" -> exactly that long before now
/// - an ISO 8601 date-time (`2026-10-18T10:00:00Z`, `…+05:30`, `…+0530`,
///   optional seconds and fractional seconds, `T` or a space) -> exactly
///   that instant; without an offset it is read as local time
///
/// `is_end_of_day`: for the whole-day forms, true means 23:59:59.999 and
/// false means 00:00:00.000. Forms that name an instant ignore it.
pub fn parse_date_input(input: &str, is_end_of_day: bool) -> Option<i64> {
    let input = input.trim().to_lowercase();

    if let Some(ms) = parse_date_time(&input) {
        return Some(ms);
    }

    let date = if input == "today" {
        Local::now().date_naive()
    } else if input == "yesterday" {
        Local::now().date_naive().pred_opt()?
    } else if let Some((n, unit)) = parse_relative(&input) {
        // `try_*`, not the panicking constructors: an absurd count must be
        // an unparseable date, not a panic in whoever is parsing it.
        let span = match unit {
            RelativeUnit::Minutes => TimeDelta::try_minutes(n)?,
            RelativeUnit::Hours => TimeDelta::try_hours(n)?,
            RelativeUnit::Days => TimeDelta::try_days(n)?,
            RelativeUnit::Weeks => TimeDelta::try_weeks(n)?,
        };
        if matches!(unit, RelativeUnit::Minutes | RelativeUnit::Hours) {
            return Some(Local::now().checked_sub_signed(span)?.timestamp_millis());
        }
        Local::now().date_naive().checked_sub_signed(span)?
    } else {
        NaiveDate::parse_from_str(&input, "%Y-%m-%d").ok()?
    };

    let time = if is_end_of_day {
        NaiveTime::from_hms_milli_opt(23, 59, 59, 999)?
    } else {
        NaiveTime::from_hms_milli_opt(0, 0, 0, 0)?
    };

    let dt = date.and_time(time);
    let dt_local = resolve_local(|naive| Local.from_local_datetime(&naive), dt, is_end_of_day)?;

    Some(dt_local.timestamp_millis())
}

/// A date given on the command line as `flag` (`--after`, `--before`), read
/// as [`parse_date_input`] reads it. Absent is `Ok(None)`; present but
/// unreadable is an error naming the accepted formats, never a filter
/// silently dropped, which would return more than was asked for.
pub fn parse_date_flag(
    flag: &str,
    input: Option<&str>,
    is_end_of_day: bool,
) -> Result<Option<i64>, String> {
    input.map_or(Ok(None), |text| {
        parse_date_input(text, is_end_of_day)
            .map(Some)
            .ok_or_else(|| {
                format!(
                    "could not read {flag} \"{text}\" as a date. Accepted: {DATE_INPUT_FORMATS}"
                )
            })
    })
}

/// An ISO 8601 date-time, with or without an offset, as epoch milliseconds.
/// Expects lowercased input, which chrono's RFC 3339 parser accepts.
fn parse_date_time(input: &str) -> Option<i64> {
    if let Ok(dt) = DateTime::parse_from_rfc3339(input) {
        return Some(dt.timestamp_millis());
    }
    // RFC 3339 without seconds, or with a `+0530`-style offset.
    for format in [
        "%Y-%m-%dt%H:%M%#z",
        "%Y-%m-%dt%H:%M:%S%.f%#z",
        "%Y-%m-%d %H:%M:%S%.f%#z",
    ] {
        let candidate = input
            .strip_suffix('z')
            .map_or_else(|| input.to_string(), |rest| format!("{rest}+00:00"));
        if let Ok(dt) = DateTime::parse_from_str(&candidate, format) {
            return Some(dt.timestamp_millis());
        }
    }
    for format in [
        "%Y-%m-%dt%H:%M:%S%.f",
        "%Y-%m-%dt%H:%M",
        "%Y-%m-%d %H:%M:%S%.f",
        "%Y-%m-%d %H:%M",
    ] {
        if let Ok(naive) = NaiveDateTime::parse_from_str(input, format) {
            let local = resolve_local(|n| Local.from_local_datetime(&n), naive, false)?;
            return Some(local.timestamp_millis());
        }
    }
    None
}

/// Pin a local wall-clock time to one instant, even across a clock change.
///
/// When the time occurs twice (clocks went back), a start takes the first
/// occurrence and an end the second, so the day keeps every instant that
/// belongs to it. When it never occurs (clocks went forward over it), a
/// start moves forward and an end moves back, a minute at a time, to the
/// nearest wall-clock time that exists.
fn resolve_local<T>(
    lookup: impl Fn(NaiveDateTime) -> LocalResult<T>,
    dt: NaiveDateTime,
    is_end: bool,
) -> Option<T> {
    // No clock change skips more than a few hours of wall time.
    const MAX_GAP_MINUTES: i64 = 24 * 60;
    let pick = |result: LocalResult<T>| match result {
        LocalResult::Single(t) => Some(t),
        LocalResult::Ambiguous(first, second) => Some(if is_end { second } else { first }),
        LocalResult::None => None,
    };
    if let Some(t) = pick(lookup(dt)) {
        return Some(t);
    }
    let step = TimeDelta::try_minutes(if is_end { -1 } else { 1 })?;
    let mut candidate = dt;
    for _ in 0..MAX_GAP_MINUTES {
        candidate = candidate.checked_add_signed(step)?;
        if let Some(t) = pick(lookup(candidate)) {
            return Some(t);
        }
    }
    None
}

#[derive(Clone, Copy)]
enum RelativeUnit {
    Minutes,
    Hours,
    Days,
    Weeks,
}

/// Parse relative strings like "7 days ago", "1 week ago", "3 hours ago",
/// "45 minutes ago". The count must be a positive whole number.
fn parse_relative(input: &str) -> Option<(i64, RelativeUnit)> {
    let rest = input.trim().strip_suffix(" ago")?;
    let (count, unit) = rest.trim().rsplit_once(' ')?;
    let unit = match unit {
        "minute" | "minutes" => RelativeUnit::Minutes,
        "hour" | "hours" => RelativeUnit::Hours,
        "day" | "days" => RelativeUnit::Days,
        "week" | "weeks" => RelativeUnit::Weeks,
        _ => return None,
    };
    let count = count.trim().parse::<i64>().ok().filter(|&n| n > 0)?;
    Some((count, unit))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_date_flag_is_read_or_refused_with_the_accepted_formats() {
        assert_eq!(parse_date_flag("--after", None, false), Ok(None));
        assert_eq!(
            parse_date_flag("--after", Some("2024-01-15"), false),
            Ok(parse_date_input("2024-01-15", false))
        );
        let error = parse_date_flag("--before", Some("last tuesday"), true).unwrap_err();
        assert_eq!(
            error,
            format!("could not read --before \"last tuesday\" as a date. Accepted: {DATE_INPUT_FORMATS}")
        );
    }

    #[test]
    fn test_parse_date_iso() {
        let ts = parse_date_input("2023-01-01", false).unwrap();
        let dt = Local.timestamp_millis_opt(ts).unwrap();
        assert_eq!(
            dt.format("%Y-%m-%d %H:%M:%S").to_string(),
            "2023-01-01 00:00:00"
        );
    }

    #[test]
    fn test_parse_keywords() {
        assert!(parse_date_input("today", false).is_some());
        assert!(parse_date_input("yesterday", true).is_some());
    }

    #[test]
    fn test_parse_relative_days() {
        assert!(parse_date_input("7 days ago", false).is_some());
        assert!(parse_date_input("1 day ago", false).is_some());
        assert!(parse_date_input("30 days ago", false).is_some());
        assert!(parse_date_input("3 days ago", true).is_some());

        // 7 days ago should be before yesterday
        let seven = parse_date_input("7 days ago", false).unwrap();
        let yesterday = parse_date_input("yesterday", false).unwrap();
        assert!(seven < yesterday);
    }

    #[test]
    fn a_relative_date_too_far_back_is_rejected_instead_of_panicking() {
        assert!(parse_date_input("200000000000 days ago", false).is_none());
        assert!(parse_date_input("9223372036854775807 days ago", true).is_none());
        assert!(parse_date_input("100000000 days ago", false).is_none());
    }

    fn utc_ms(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> i64 {
        chrono::Utc
            .with_ymd_and_hms(y, mo, d, h, mi, s)
            .unwrap()
            .timestamp_millis()
    }

    #[test]
    fn iso_8601_date_times_parse_to_that_exact_instant() {
        let ten_utc = utc_ms(2026, 10, 18, 10, 0, 0);
        for end_of_day in [false, true] {
            assert_eq!(
                parse_date_input("2026-10-18T10:00:00Z", end_of_day),
                Some(ten_utc)
            );
            assert_eq!(
                parse_date_input("2026-10-18t10:00:00z", end_of_day),
                Some(ten_utc)
            );
            assert_eq!(
                parse_date_input("2026-10-18T15:30:00+05:30", end_of_day),
                Some(ten_utc)
            );
            assert_eq!(
                parse_date_input("2026-10-18T06:00:00-04:00", end_of_day),
                Some(ten_utc)
            );
            assert_eq!(
                parse_date_input("2026-10-18T15:30:00+0530", end_of_day),
                Some(ten_utc)
            );
            assert_eq!(
                parse_date_input("2026-10-18T10:00Z", end_of_day),
                Some(ten_utc)
            );
            assert_eq!(
                parse_date_input("2026-10-18T10:00:00.250Z", end_of_day),
                Some(ten_utc + 250)
            );
        }
    }

    #[test]
    fn iso_8601_date_times_without_an_offset_are_local_time() {
        let expected = Local
            .with_ymd_and_hms(2026, 10, 18, 10, 0, 0)
            .earliest()
            .unwrap()
            .timestamp_millis();
        assert_eq!(
            parse_date_input("2026-10-18T10:00:00", false),
            Some(expected)
        );
        assert_eq!(parse_date_input("2026-10-18T10:00", true), Some(expected));
        assert_eq!(
            parse_date_input("2026-10-18 10:00:00", false),
            Some(expected)
        );
    }

    #[test]
    fn minutes_hours_and_weeks_ago_parse() {
        let now = chrono::Utc::now().timestamp_millis();
        let close = |got: Option<i64>, expected: i64| {
            let got = got.expect("should parse");
            assert!((got - expected).abs() < 5_000, "{got} vs {expected}");
        };
        close(parse_date_input("3 hours ago", false), now - 3 * 3_600_000);
        close(parse_date_input("1 hour ago", true), now - 3_600_000);
        close(parse_date_input("45 minutes ago", false), now - 45 * 60_000);
        close(parse_date_input("1 minute ago", false), now - 60_000);
        assert_eq!(
            parse_date_input("2 weeks ago", false),
            parse_date_input("14 days ago", false)
        );
        assert_eq!(
            parse_date_input("1 week ago", true),
            parse_date_input("7 days ago", true)
        );
        assert!(parse_date_input("99999999999999 hours ago", false).is_none());
        assert!(parse_date_input("9999999999999999 minutes ago", false).is_none());
        assert!(parse_date_input("99999999999999 weeks ago", false).is_none());
    }

    #[test]
    fn text_that_is_not_a_date_does_not_parse() {
        for input in [
            "last tuesday",
            "2026-13-01",
            "2026-10-18T25:00:00Z",
            "5 fortnights ago",
            "3 hours",
            "",
        ] {
            assert!(parse_date_input(input, false).is_none(), "{input}");
        }
    }

    #[test]
    fn an_ambiguous_local_midnight_takes_the_earlier_start_and_the_later_end() {
        let dt = NaiveDate::from_ymd_opt(2026, 11, 1)
            .unwrap()
            .and_hms_opt(0, 0, 0)
            .unwrap();
        let lookup = |_: NaiveDateTime| LocalResult::Ambiguous(1, 2);
        assert_eq!(resolve_local(lookup, dt, false), Some(1));
        assert_eq!(resolve_local(lookup, dt, true), Some(2));
    }

    #[test]
    fn a_local_midnight_skipped_by_a_clock_change_falls_forward_to_the_first_real_instant() {
        let day = NaiveDate::from_ymd_opt(2026, 3, 8).unwrap();
        let midnight = day.and_hms_opt(0, 0, 0).unwrap();
        let one_am = day.and_hms_opt(1, 0, 0).unwrap();
        // Midnight does not exist; 01:00 is the first wall-clock time that does.
        let lookup = |dt: NaiveDateTime| {
            if dt < one_am {
                LocalResult::None
            } else {
                LocalResult::Single(dt)
            }
        };
        assert_eq!(resolve_local(lookup, midnight, false), Some(one_am));

        // A skipped end-of-day falls back to the last real instant before it.
        let end = day.and_hms_milli_opt(23, 59, 59, 999).unwrap();
        let gap_start = day.and_hms_opt(23, 0, 0).unwrap();
        let lookup = |dt: NaiveDateTime| {
            if dt >= gap_start {
                LocalResult::None
            } else {
                LocalResult::Single(dt)
            }
        };
        let resolved = resolve_local(lookup, end, true).unwrap();
        assert!(resolved < gap_start, "{resolved}");
    }

    #[test]
    fn test_parse_relative_invalid() {
        assert!(parse_date_input("0 days ago", false).is_none());
        assert!(parse_date_input("-1 days ago", false).is_none());
        assert!(parse_date_input("days ago", false).is_none());
        assert!(parse_date_input("seven days ago", false).is_none());
    }
}
