//! The one response convention every MCP tool and resource follows.
//!
//! Before PROD-13 each handler invented its own: three timestamp formats,
//! `?` / `-1` / a silent `0` for the same missing exit code, silent
//! truncation in some tools and `... and N more` in others, pagination in
//! two tools out of twenty, and no way at all for a caller to tell a
//! recorded fact from something suvadu guessed. An agent cannot calibrate
//! how much to trust a response that changes shape per tool, so the rules
//! below are stated once, implemented once here, and asserted over the
//! whole tool surface in [`super::contract`].
//!
//! # The rules
//!
//! 1. **Timestamps.** Every absolute time is RFC 3339 with an explicit
//!    offset (`2026-09-19T09:00:00+05:30`), rendered by [`timestamp`]. A
//!    relative hint may *follow* it in parentheses ([`when`]); it never
//!    appears alone. A time suvadu does not have is [`UNKNOWN`]. JSON
//!    responses keep raw epoch milliseconds and declare that in their
//!    `provenance.time_unit` ([`JSON_TIME_UNIT`]), so the two surfaces are
//!    each unambiguous without converting the other's values.
//! 2. **Units.** Durations always carry a unit ([`duration`]). Time windows
//!    are written `last 4h` / `last 7d` ([`window_hours`], [`window_days`]).
//!    A percentage always travels with the fraction it came from
//!    ([`rate`]): `3/7 (43%)`, never a bare `43%`.
//! 3. **Unknown values.** One token, [`UNKNOWN`]. Never `?`, `-1`, `n/a`,
//!    an empty string, or a `0` standing in for "not recorded".
//! 4. **Pagination.** Every list-shaped tool takes `limit` and `offset`,
//!    and every response's trailer carries `next_offset:` — an integer to
//!    pass back, or `none` when the listing is exhausted.
//! 5. **Truncation.** Nothing is dropped silently. Elided rows are counted
//!    in `shown:` and announced with [`more_not_shown`]; an over-long
//!    string ends in `…` ([`clip`]).
//! 6. **Provenance.** The trailer's `provenance:` line says what kind of
//!    content the response holds — [`Provenance::Observed`] for records
//!    suvadu stored, [`Provenance::Inferred`] for anything derived from
//!    them (classification, ranking, rates, predictions),
//!    [`Provenance::CallerReported`] for text an agent wrote. Anything
//!    inferred also carries [`NO_OUTPUT_NOTE`], because the single most
//!    common way to over-trust suvadu is to read a classified command as
//!    an observed effect.
//! 7. **Stable IDs.** Every command row starts with `command-<id>`, the
//!    same identifier `get_agent_session` returns, so a concise row can
//!    always be followed up ([`command_id`]).
//! 8. **Concise by default.** Rows are one line. `detail: true` adds
//!    directory, duration, executor, session and prompt, and every concise
//!    response says so ([`DETAIL_HINT`]).

#[cfg(test)]
use std::collections::BTreeMap;
use std::fmt::Write as _;

use chrono::TimeZone;

/// The single token for anything suvadu did not record.
pub const UNKNOWN: &str = "unknown";

/// Declared unit for every `*_at` / `*_ms` number in a JSON response.
pub const JSON_TIME_UNIT: &str = "epoch_ms_utc";

/// The one caveat that must accompany anything inferred. Suvadu records
/// that a command ran and how it exited; it never records what the command
/// printed or what it did to the filesystem.
pub const NO_OUTPUT_NOTE: &str =
    "suvadu records commands, exit codes and timings — never command output or file contents";

/// How a concise response tells a caller to ask for more.
pub const DETAIL_HINT: &str = "pass detail=true for directory, duration, executor and session";

/// Longest command text shown on one row before it is clipped.
pub const ROW_MAX_CHARS: usize = 160;

/// Longest prompt text shown before it is clipped.
pub const PROMPT_MAX_CHARS: usize = 120;

/// Where a response's content came from, as printed on the `provenance:`
/// trailer line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provenance {
    /// Records suvadu stored: command text, exit code, timing, directory,
    /// executor, captured transcript events.
    Observed,
    /// Derived from those records — a classification, a ranking, a rate, a
    /// prediction. Never itself an observation.
    Inferred,
    /// Both, in one response: the records and something computed from them.
    ObservedAndInferred,
    /// Text a caller wrote and suvadu merely stored back.
    CallerReported,
}

impl Provenance {
    /// The trailer token.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Observed => "observed",
            Self::Inferred => "inferred",
            Self::ObservedAndInferred => "observed+inferred",
            Self::CallerReported => "caller-reported",
        }
    }

    /// Whether this response must carry [`NO_OUTPUT_NOTE`].
    pub const fn needs_capture_note(self) -> bool {
        matches!(self, Self::Inferred | Self::ObservedAndInferred)
    }
}

/// RFC 3339 with an explicit offset, or [`UNKNOWN`].
pub fn timestamp(ms: i64) -> String {
    if ms <= 0 {
        return UNKNOWN.to_string();
    }
    chrono::Local
        .timestamp_millis_opt(crate::util::normalize_display_ms(ms))
        .single()
        .map_or_else(
            || UNKNOWN.to_string(),
            |dt| dt.to_rfc3339_opts(chrono::SecondsFormat::Secs, false),
        )
}

/// A relative hint like `3 hours ago`. Only ever shown next to an absolute
/// timestamp, never instead of one.
pub fn relative(ms: i64) -> String {
    if ms <= 0 {
        return UNKNOWN.to_string();
    }
    let diff = chrono::Utc::now().timestamp_millis() - crate::util::normalize_display_ms(ms);
    if diff < 0 {
        return "just now".to_string();
    }
    let minutes = diff / 60_000;
    let hours = minutes / 60;
    let days = hours / 24;
    let plural = |n: i64| if n == 1 { "" } else { "s" };
    if days > 0 {
        format!("{days} day{} ago", plural(days))
    } else if hours > 0 {
        format!("{hours} hour{} ago", plural(hours))
    } else if minutes > 0 {
        format!("{minutes} minute{} ago", plural(minutes))
    } else {
        "just now".to_string()
    }
}

/// The canonical rendering of a moment: absolute, then the relative hint.
pub fn when(ms: i64) -> String {
    if ms <= 0 {
        return UNKNOWN.to_string();
    }
    format!("{} ({})", timestamp(ms), relative(ms))
}

/// A duration that always carries its unit.
pub fn duration(ms: i64) -> String {
    if ms <= 0 {
        return UNKNOWN.to_string();
    }
    crate::util::format_duration_ms(ms)
}

/// `last 4h`, with the unit spelled out.
pub fn window_hours(hours: i64) -> String {
    format!("last {hours}h")
}

/// `last 7d`, with the unit spelled out.
pub fn window_days(days: i64) -> String {
    format!("last {days}d")
}

/// An exit code as `ok`, `exit 101`, or `exit unknown` — never `?` or `-1`.
pub fn exit(code: Option<i32>) -> String {
    match code {
        Some(0) => "ok".to_string(),
        Some(c) => format!("exit {c}"),
        None => format!("exit {UNKNOWN}"),
    }
}

/// An optional field, or [`UNKNOWN`]. An empty string counts as missing.
pub const fn or_unknown(value: Option<&str>) -> &str {
    match value {
        Some(v) if !v.is_empty() => v,
        _ => UNKNOWN,
    }
}

/// `3/7 (43%)`. A ratio with no denominator is `0/0 (unknown)` rather than
/// a misleading `0%`.
pub fn rate(part: usize, total: usize) -> String {
    if total == 0 {
        return format!("0/0 ({UNKNOWN})");
    }
    format!("{part}/{total} ({}%)", part * 100 / total)
}

/// Clip a string to `max` characters, marking it with `…` when shortened.
pub fn clip(text: &str, max: usize) -> String {
    let collapsed = if text.contains('\n') {
        text.split_whitespace().collect::<Vec<_>>().join(" ")
    } else {
        text.to_string()
    };
    crate::util::truncate_str(&collapsed, max, "…")
}

/// The stable, cross-tool identifier for one recorded command — the same
/// one `get_agent_session` returns for it.
pub fn command_id(id: Option<i64>) -> String {
    id.map_or_else(
        || format!("command-{UNKNOWN}"),
        |id| format!("command-{id}"),
    )
}

/// The one way a response admits it elided rows.
pub fn more_not_shown(count: usize) -> String {
    format!("… {count} more not shown")
}

/// A builder that guarantees the trailer is present and well formed.
pub struct Response {
    head: String,
    body: String,
    shown: usize,
    matched: Option<usize>,
    next_offset: Option<usize>,
    provenance: Provenance,
    notes: Vec<String>,
}

impl Response {
    /// Start a response with its one-line header.
    pub fn new(head: impl Into<String>, provenance: Provenance) -> Self {
        Self {
            head: head.into(),
            body: String::new(),
            shown: 0,
            matched: None,
            next_offset: None,
            provenance,
            notes: Vec::new(),
        }
    }

    /// How many records this response actually shows.
    pub const fn shown(mut self, shown: usize) -> Self {
        self.shown = shown;
        self
    }

    /// How many records matched in total, when that is known.
    pub const fn matched(mut self, matched: usize) -> Self {
        self.matched = Some(matched);
        self
    }

    /// The offset to pass back for the next page, when there is one.
    pub const fn next_offset(mut self, next: Option<usize>) -> Self {
        self.next_offset = next;
        self
    }

    /// Add a caveat line to the trailer.
    pub fn note(mut self, note: impl Into<String>) -> Self {
        let note = note.into();
        if !self.notes.contains(&note) {
            self.notes.push(note);
        }
        self
    }

    /// Append a body line.
    pub fn line(&mut self, line: impl AsRef<str>) {
        self.body.push_str(line.as_ref());
        self.body.push('\n');
    }

    /// Append a blank separator line, collapsing consecutive blanks.
    pub fn blank(&mut self) {
        if !self.body.is_empty() && !self.body.ends_with("\n\n") {
            self.body.push('\n');
        }
    }

    /// Render header, body and the mandatory trailer.
    pub fn render(mut self) -> String {
        if self.provenance.needs_capture_note() {
            self = self.note(NO_OUTPUT_NOTE);
        }
        let mut out = self.head.clone();
        out.push('\n');
        if !self.body.is_empty() {
            out.push('\n');
            out.push_str(self.body.trim_end_matches('\n'));
            out.push('\n');
        }
        out.push_str("\n---\n");
        let matched = self
            .matched
            .map_or_else(|| UNKNOWN.to_string(), |m| m.to_string());
        let _ = writeln!(out, "shown: {} of {matched}", self.shown);
        let _ = writeln!(
            out,
            "next_offset: {}",
            self.next_offset
                .map_or_else(|| "none".to_string(), |n| n.to_string())
        );
        let _ = writeln!(out, "provenance: {}", self.provenance.label());
        for note in &self.notes {
            let _ = writeln!(out, "note: {note}");
        }
        out
    }
}

// ── Parsing helpers, for the contract fixtures ──────────────

/// Parse the trailer of a rendered response into `key -> value`. `None`
/// when the response has no trailer at all, which is itself a contract
/// violation.
#[cfg(test)]
pub fn parse_trailer(text: &str) -> Option<BTreeMap<String, String>> {
    let (_, trailer) = text.rsplit_once("\n---\n")?;
    let mut map = BTreeMap::new();
    for line in trailer.lines() {
        if let Some((key, value)) = line.split_once(": ") {
            map.entry(key.trim().to_string())
                .or_insert_with(|| value.trim().to_string());
        }
    }
    Some(map)
}

/// Every `command-<id>` mentioned in a response, in order.
#[cfg(test)]
pub fn command_ids(text: &str) -> Vec<String> {
    text.split_whitespace()
        .filter_map(|token| {
            let token = token.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '-');
            let rest = token.strip_prefix("command-")?;
            (!rest.is_empty() && rest.chars().all(|c| c.is_ascii_digit()))
                .then(|| token.to_string())
        })
        .collect()
}

/// True when the text still contains the old space-separated local
/// timestamp (`2026-09-19 09:00:00`) that rule 1 replaced.
#[cfg(test)]
pub fn has_legacy_timestamp(text: &str) -> bool {
    let bytes: Vec<char> = text.chars().collect();
    bytes.windows(19).any(|w| {
        w[..4].iter().all(char::is_ascii_digit)
            && w[4] == '-'
            && w[5..7].iter().all(char::is_ascii_digit)
            && w[7] == '-'
            && w[8..10].iter().all(char::is_ascii_digit)
            && w[10] == ' '
            && w[11..13].iter().all(char::is_ascii_digit)
            && w[13] == ':'
            && w[14..16].iter().all(char::is_ascii_digit)
            && w[16] == ':'
            && w[17..19].iter().all(char::is_ascii_digit)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknowns_never_render_as_zero_or_a_question_mark() {
        assert_eq!(exit(None), "exit unknown");
        assert_eq!(exit(Some(0)), "ok");
        assert_eq!(exit(Some(2)), "exit 2");
        assert_eq!(timestamp(0), UNKNOWN);
        assert_eq!(duration(0), UNKNOWN);
        assert_eq!(or_unknown(Some("")), UNKNOWN);
        assert_eq!(or_unknown(None), UNKNOWN);
        assert_eq!(rate(0, 0), "0/0 (unknown)");
        assert_eq!(rate(3, 7), "3/7 (42%)");
        assert_eq!(command_id(None), "command-unknown");
        assert_eq!(command_id(Some(7)), "command-7");
    }

    #[test]
    fn timestamps_carry_an_offset_and_relative_hints_never_stand_alone() {
        let rendered = when(chrono::Utc::now().timestamp_millis() - 3_600_000);
        assert!(rendered.contains("hour"), "{rendered}");
        assert!(!has_legacy_timestamp(&rendered), "{rendered}");
        assert!(has_legacy_timestamp("ran at 2026-09-19 09:00:00 today"));
    }

    #[test]
    fn the_trailer_is_always_present_and_parseable() {
        let mut response = Response::new("2 commands", Provenance::Observed);
        response.line("command-1 | ok | echo hi");
        let text = response.shown(2).matched(9).next_offset(Some(2)).render();
        let trailer = parse_trailer(&text).unwrap();
        assert_eq!(trailer["shown"], "2 of 9");
        assert_eq!(trailer["next_offset"], "2");
        assert_eq!(trailer["provenance"], "observed");
        assert_eq!(command_ids(&text), vec!["command-1".to_string()]);
    }

    #[test]
    fn anything_inferred_admits_that_output_was_never_captured() {
        let text = Response::new("changes", Provenance::Inferred).render();
        assert!(text.contains(NO_OUTPUT_NOTE), "{text}");
        let observed = Response::new("commands", Provenance::Observed).render();
        assert!(!observed.contains(NO_OUTPUT_NOTE), "{observed}");
        assert_eq!(
            parse_trailer(&observed).unwrap()["next_offset"],
            "none",
            "{observed}"
        );
    }

    #[test]
    fn clipping_marks_what_it_removed() {
        let clipped = clip(&"x".repeat(400), ROW_MAX_CHARS);
        assert!(clipped.ends_with('…'), "{clipped}");
        assert!(clipped.chars().count() <= ROW_MAX_CHARS);
        assert_eq!(more_not_shown(4), "… 4 more not shown");
    }
}
