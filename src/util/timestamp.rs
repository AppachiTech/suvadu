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
}
