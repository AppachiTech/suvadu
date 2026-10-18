use chrono::{Local, NaiveDate, NaiveTime, TimeZone};

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

/// Parse a date string input into a Unix timestamp (milliseconds).
///
/// Supported formats:
/// - "YYYY-MM-DD" -> Returns timestamp at given `time_of_day`
/// - "today" -> Returns today at `time_of_day`
/// - "yesterday" -> Returns yesterday at `time_of_day`
///
/// `is_end_of_day`: If true, defaults to 23:59:59.999. If false, 00:00:00.000.
pub fn parse_date_input(input: &str, is_end_of_day: bool) -> Option<i64> {
    let input = input.trim().to_lowercase();

    let date = if input == "today" {
        Local::now().date_naive()
    } else if input == "yesterday" {
        Local::now().date_naive().pred_opt()?
    } else if let Some(days) = parse_relative_days(&input) {
        // `try_days`, not `days`: an absurd count must be an unparseable
        // date, not a panic in whoever is parsing it.
        Local::now()
            .date_naive()
            .checked_sub_signed(chrono::TimeDelta::try_days(days)?)?
    } else {
        NaiveDate::parse_from_str(&input, "%Y-%m-%d").ok()?
    };

    let time = if is_end_of_day {
        NaiveTime::from_hms_milli_opt(23, 59, 59, 999)?
    } else {
        NaiveTime::from_hms_milli_opt(0, 0, 0, 0)?
    };

    let dt = date.and_time(time);
    let dt_local = Local.from_local_datetime(&dt).single()?;

    Some(dt_local.timestamp_millis())
}

/// Parse relative date strings like "7 days ago", "3 days ago", "1 day ago".
fn parse_relative_days(input: &str) -> Option<i64> {
    let input = input.trim();
    // Match: "<N> day(s) ago"
    let rest = input.strip_suffix(" ago")?;
    let rest = rest
        .strip_suffix(" days")
        .or_else(|| rest.strip_suffix(" day"))?;
    rest.trim().parse::<i64>().ok().filter(|&n| n > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn test_parse_relative_invalid() {
        assert!(parse_date_input("0 days ago", false).is_none());
        assert!(parse_date_input("-1 days ago", false).is_none());
        assert!(parse_date_input("days ago", false).is_none());
        assert!(parse_date_input("seven days ago", false).is_none());
    }
}
