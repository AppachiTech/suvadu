//! The end of what a failed command printed, made safe to store.
//!
//! Off by default (`[capture] failure_output`). Only the capture paths that
//! actually see a command's output feed this: agent hooks whose payload
//! carries it, and `suv wrap`, which runs the command itself. The zsh and
//! Bash hooks never do: capturing a terminal's output means standing between
//! the program and the terminal, which breaks anything interactive.
//!
//! Every path goes through [`finish`], in this order:
//! 1. the caller keeps at most the last [`WINDOW_BYTES`] of the raw output
//!    ([`Captured::from_text`] or [`TailBuffer`]), so a 16 MB build log
//!    costs neither memory nor regex time;
//! 2. terminal escape sequences and carriage-return redraws are removed;
//! 3. the whole window is redacted with the same rules as command text;
//! 4. only then is it cut to the last [`TAIL_BYTES`], at a line start.
//!
//! Redacting before cutting means a secret that crosses the cut is already
//! replaced; one that crossed the window's start is at least
//! `WINDOW_BYTES - TAIL_BYTES` from anything kept, and the window's first,
//! partial line is dropped as well.

use crate::models::FailureOutput;

/// Most bytes of cleaned output kept per command.
pub const TAIL_BYTES: usize = 4096;

/// Most bytes of raw output looked at per command: what is cleaned and
/// redacted before the tail is cut from it.
pub const WINDOW_BYTES: usize = 64 * 1024;

/// One stream of output, as captured, before it is cleaned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Captured {
    /// See [`FailureOutput::stream`].
    pub stream: &'static str,
    /// At most [`WINDOW_BYTES`] of the end of the output.
    pub text: String,
    /// Bytes of output before `text` that were not kept.
    pub dropped_bytes: u64,
}

impl Captured {
    /// The last [`WINDOW_BYTES`] of `text`, cut on a character boundary.
    pub fn from_text(stream: &'static str, text: &str) -> Self {
        let mut start = text.len().saturating_sub(WINDOW_BYTES);
        while !text.is_char_boundary(start) {
            start += 1;
        }
        Self {
            stream,
            text: text[start..].to_string(),
            dropped_bytes: start as u64,
        }
    }

    /// An already cut window: `dropped_bytes` came before `text`, and
    /// `text` itself is cut again if it is longer than a window.
    pub fn from_window(stream: &'static str, text: &str, dropped_bytes: u64) -> Self {
        let mut captured = Self::from_text(stream, text);
        captured.dropped_bytes += dropped_bytes;
        captured
    }
}

/// Whether a command with `exit_code` failed, for keeping its output:
/// known, not zero, and not 130 (Ctrl+C), as [`crate::frecency`] counts it.
pub fn is_failure(exit_code: Option<i32>) -> bool {
    exit_code.is_some_and(|code| code != 0 && code != crate::frecency::INTERRUPTED)
}

/// Clean, redact and cut `captured` into what is stored, or `None` when
/// nothing printable is left. `redaction` is the policy of the command's
/// own directory.
pub fn finish(
    captured: &Captured,
    redaction: &crate::config::RedactionConfig,
) -> Option<FailureOutput> {
    let window = if captured.dropped_bytes > 0 {
        drop_partial_head(&captured.text)
    } else {
        &captured.text[..]
    };
    let head_dropped = (captured.text.len() - window.len()) as u64;
    let cleaned = strip_terminal_escapes(window);
    let redacted = if redaction.enabled {
        crate::redact::redact_secrets_with_extra(&cleaned, &redaction.extra_patterns)
    } else {
        cleaned
    };
    let text = redacted.trim_end();
    let start = tail_start(text);
    let tail = text[start..].trim_start_matches(['\n', '\r']);
    if tail.trim().is_empty() {
        return None;
    }
    let cut = (text.len() - tail.len()) as u64;
    Some(FailureOutput {
        stream: captured.stream.to_string(),
        tail: tail.to_string(),
        truncated_bytes: i64::try_from(captured.dropped_bytes + head_dropped + cut)
            .unwrap_or(i64::MAX),
    })
}

/// `output` as restored from an export: kept exactly as it was stored,
/// except that a tail longer than [`TAIL_BYTES`] (a hand-edited file) is
/// cut like a captured one.
pub fn clamp(mut output: FailureOutput) -> FailureOutput {
    let start = tail_start(&output.tail);
    if start > 0 {
        output.tail.drain(..start);
        output.truncated_bytes = output
            .truncated_bytes
            .saturating_add(i64::try_from(start).unwrap_or(i64::MAX));
    }
    output
}

/// Where the last [`TAIL_BYTES`] of `text` start: at the beginning of a
/// line when one begins inside them, otherwise (a single line longer than
/// that) on a character boundary.
fn tail_start(text: &str) -> usize {
    if text.len() <= TAIL_BYTES {
        return 0;
    }
    let mut start = text.len() - TAIL_BYTES;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    if text.as_bytes()[start - 1] == b'\n' {
        return start;
    }
    match text[start..].find('\n') {
        Some(newline) if start + newline + 1 < text.len() => start + newline + 1,
        _ => start,
    }
}

/// A window cut from the middle of the output starts part-way through a
/// line, which may be the end of a secret whose start was cut off. Drop
/// that line, and the rest of a private-key block it may have belonged to:
/// base64 lines and the block's `-----END` marker.
fn drop_partial_head(text: &str) -> &str {
    let Some(newline) = text.find('\n') else {
        return "";
    };
    let mut rest = &text[newline + 1..];
    loop {
        let (line, after) = rest.split_once('\n').unwrap_or((rest, ""));
        let line = line.trim_end_matches('\r');
        let base64 = line.len() >= 16
            && line
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'='));
        let end_marker = line.starts_with("-----END ") && line.ends_with("-----");
        if !(base64 || end_marker) || rest.is_empty() {
            return rest;
        }
        rest = after;
        if end_marker {
            return rest;
        }
    }
}

/// `text` as a terminal would leave it, as plain lines: ANSI/VT escape
/// sequences (CSI, OSC, DCS and the other string sequences, and two-byte
/// escapes) and other control characters removed, `\r\n` read as a line
/// end, and a line redrawn after a bare `\r` (a progress bar) kept only as
/// it was last drawn. Tabs stay.
pub fn strip_terminal_escapes(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut line_start = 0;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => skip_escape(&mut chars),
            '\u{9b}' => skip_csi(&mut chars),
            '\n' => {
                out.push('\n');
                line_start = out.len();
            }
            '\r' => {
                if chars.peek() != Some(&'\n') {
                    out.truncate(line_start);
                }
            }
            '\t' => out.push('\t'),
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

/// Skip the rest of an escape sequence whose `ESC` was just read.
fn skip_escape(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    match chars.next() {
        Some('[') => skip_csi(chars),
        // OSC, DCS, SOS, PM and APC: a string up to BEL or `ESC \`.
        Some(']' | 'P' | 'X' | '^' | '_') => {
            while let Some(c) = chars.next() {
                if c == '\u{7}' {
                    return;
                }
                if c == '\u{1b}' {
                    if chars.peek() == Some(&'\\') {
                        chars.next();
                    }
                    return;
                }
            }
        }
        // `ESC ( B` and friends: intermediates, then one final byte.
        Some(c) if ('\u{20}'..='\u{2f}').contains(&c) => {
            for c in chars.by_ref() {
                if !('\u{20}'..='\u{2f}').contains(&c) {
                    return;
                }
            }
        }
        _ => {}
    }
}

/// Skip a control sequence's parameters, intermediates and final byte.
fn skip_csi(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    for c in chars.by_ref() {
        if ('\u{40}'..='\u{7e}').contains(&c) {
            return;
        }
        if !('\u{20}'..='\u{3f}').contains(&c) {
            return;
        }
    }
}

/// The last [`WINDOW_BYTES`] of a byte stream, kept while the stream is
/// passed on: what `suv wrap` holds of a command's stderr.
#[derive(Debug, Default)]
pub struct TailBuffer {
    buf: std::collections::VecDeque<u8>,
    total: u64,
}

impl TailBuffer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append `bytes`, forgetting what falls out of the window.
    pub fn push(&mut self, bytes: &[u8]) {
        self.total += bytes.len() as u64;
        let bytes = &bytes[bytes.len().saturating_sub(WINDOW_BYTES)..];
        let overflow = (self.buf.len() + bytes.len()).saturating_sub(WINDOW_BYTES);
        self.buf.drain(..overflow);
        self.buf.extend(bytes);
    }

    /// What was kept, as text: invalid UTF-8 is replaced, and a character
    /// the window cut through is dropped rather than half-kept.
    pub fn into_captured(self, stream: &'static str) -> Captured {
        let total = self.total;
        let mut bytes = Vec::from(self.buf);
        let dropped = total - bytes.len() as u64;
        if dropped > 0 {
            // Continuation bytes of a character that started before the
            // window.
            let skip = bytes
                .iter()
                .take(3)
                .take_while(|b| (**b & 0b1100_0000) == 0b1000_0000)
                .count();
            bytes.drain(..skip);
        }
        let skipped = total - dropped - bytes.len() as u64;
        Captured {
            stream,
            text: String::from_utf8_lossy(&bytes).into_owned(),
            dropped_bytes: dropped + skipped,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RedactionConfig;
    use std::fmt::Write as _;

    fn redaction() -> RedactionConfig {
        RedactionConfig::default()
    }

    fn finish_text(text: &str) -> Option<FailureOutput> {
        finish(&Captured::from_text("stderr", text), &redaction())
    }

    #[test]
    fn a_short_error_is_kept_whole() {
        let out = finish_text("Compiling app\nerror[E0425]: cannot find value `x`\n").unwrap();
        assert_eq!(out.stream, "stderr");
        assert_eq!(
            out.tail,
            "Compiling app\nerror[E0425]: cannot find value `x`"
        );
        assert_eq!(out.truncated_bytes, 0);
    }

    #[test]
    fn nothing_printable_stores_nothing() {
        assert_eq!(finish_text(""), None);
        assert_eq!(finish_text("\n\n  \n"), None);
        assert_eq!(finish_text("\u{1b}[31m\u{1b}[0m\n"), None);
    }

    #[test]
    fn only_a_known_non_zero_exit_that_is_not_ctrl_c_is_a_failure() {
        assert!(is_failure(Some(1)));
        assert!(is_failure(Some(127)));
        assert!(!is_failure(Some(0)));
        assert!(!is_failure(Some(130)));
        assert!(!is_failure(None));
    }

    #[test]
    fn colours_cursor_moves_titles_and_progress_redraws_are_removed() {
        let raw = "\u{1b}]0;cargo build\u{7}\u{1b}[1m\u{1b}[31merror\u{1b}[0m: failed\r\n\
                   [==>   ] 10%\r[=====>] 90%\r[======] done\n\
                   \u{1b}(Bplain\u{1b}[2K\u{1b}[1G line\tend\u{7}\n";
        assert_eq!(
            strip_terminal_escapes(raw),
            "error: failed\n[======] done\nplain line\tend\n"
        );
        let out = finish_text(raw).unwrap();
        assert!(!out.tail.contains('\u{1b}'), "{:?}", out.tail);
    }

    #[test]
    fn a_secret_in_the_output_is_redacted() {
        let raw = "fatal: Authentication failed for \
                   'https://deploy:ghp_aBcDeFgHiJkLmNoPqRsTuVwXyZ0123456789@github.com/org/repo.git'\n\
                   export AWS_SECRET_ACCESS_KEY=wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY\n";
        let out = finish_text(raw).unwrap();
        assert!(!out.tail.contains("ghp_aBcDeF"), "{}", out.tail);
        assert!(!out.tail.contains("wJalrXUtnFEMI"), "{}", out.tail);
        assert!(out.tail.contains("Authentication failed"), "{}", out.tail);
    }

    #[test]
    fn redaction_follows_the_directory_policy() {
        let raw = "token ACME-123456789 rejected\n";
        let custom = RedactionConfig {
            enabled: true,
            extra_patterns: vec!["ACME-[0-9]+".to_string()],
        };
        let out = finish(&Captured::from_text("output", raw), &custom).unwrap();
        assert!(!out.tail.contains("ACME-123456789"), "{}", out.tail);
        let off = RedactionConfig {
            enabled: false,
            extra_patterns: vec![],
        };
        let out = finish(&Captured::from_text("output", raw), &off).unwrap();
        assert!(out.tail.contains("ACME-123456789"));
    }

    #[test]
    fn a_secret_across_the_cut_is_redacted_before_the_tail_is_cut() {
        // The key straddles where a 4 KB cut would fall: it is redacted
        // whole first, so no fragment of it can survive the cut.
        let key = "ghp_aBcDeFgHiJkLmNoPqRsTuVwXyZ0123456789";
        // One line, so the cut falls inside it: 20 bytes from the key's end.
        let raw = format!(
            "{} token={key} {}",
            "x".repeat(100),
            "y".repeat(TAIL_BYTES - 21)
        );
        let out = finish_text(&raw).unwrap();
        assert!(
            out.tail.contains("***REDACTED***"),
            "the cut moved: {}",
            &out.tail[..40]
        );
        for i in 8..=key.len() {
            assert!(!out.tail.contains(&key[i - 8..i]), "{}", &out.tail[..40]);
        }
        // Cut first, the key's end would have been kept.
        let unredacted = finish(
            &Captured::from_text("stderr", &raw),
            &RedactionConfig {
                enabled: false,
                extra_patterns: vec![],
            },
        )
        .unwrap();
        assert!(
            unredacted.tail.contains(&key[key.len() - 12..]),
            "{}",
            &unredacted.tail[..40]
        );
    }

    #[test]
    fn a_long_output_keeps_its_last_whole_lines() {
        let raw: String = (0..2000).fold(String::new(), |mut raw, i| {
            let _ = writeln!(raw, "line {i:04}");
            raw
        });
        let out = finish_text(&raw).unwrap();
        assert!(out.tail.len() <= TAIL_BYTES);
        assert!(out.tail.starts_with("line "), "{:?}", &out.tail[..20]);
        assert!(out.tail.ends_with("line 1999"));
        let dropped = raw.trim_end().len() - out.tail.len();
        assert_eq!(out.truncated_bytes, i64::try_from(dropped).unwrap());
    }

    #[test]
    fn a_single_long_line_is_cut_on_a_character_boundary() {
        let raw = "é".repeat(TAIL_BYTES);
        let out = finish_text(&raw).unwrap();
        assert!(out.tail.len() <= TAIL_BYTES);
        assert!(out.tail.chars().all(|c| c == 'é'));
    }

    #[test]
    fn a_huge_output_is_windowed_and_counts_what_it_left_out() {
        let raw = format!("{}\nerror: the end\n", "a".repeat(10 * WINDOW_BYTES));
        let captured = Captured::from_text("stderr", &raw);
        assert!(captured.text.len() <= WINDOW_BYTES);
        let out = finish(&captured, &redaction()).unwrap();
        assert_eq!(out.tail, "error: the end");
        assert_eq!(
            out.truncated_bytes,
            i64::try_from(raw.trim_end().len() - out.tail.len()).unwrap()
        );
    }

    #[test]
    fn a_window_that_starts_inside_a_private_key_drops_the_rest_of_it() {
        let body: String = (0..2000)
            .map(|_| "MIIEpAIBAAKCAQEA3Tz2mr7SZiAMfQyuvBjM\n")
            .collect();
        let raw = format!(
            "-----BEGIN RSA PRIVATE KEY-----\n{body}-----END RSA PRIVATE KEY-----\nerror: bad key\n"
        );
        assert!(raw.len() > WINDOW_BYTES);
        let out = finish_text(&raw).unwrap();
        assert_eq!(out.tail, "error: bad key");
    }

    /// What cleaning and redacting a full window costs a hook, on a build
    /// log with colour codes. Prints rather than asserts. Run with
    /// `cargo test --release --bin suv failure_output_cost -- --ignored --nocapture`.
    #[test]
    #[ignore = "timing; run explicitly with --ignored"]
    fn failure_output_cost_on_a_full_window() {
        let line = "\u{1b}[1m\u{1b}[32m   Compiling\u{1b}[0m serde_json v1.0.140 \
                    (/Users/dev/.cargo/registry/src/index.crates.io/serde_json-1.0.140)\n";
        let raw = line.repeat(10 * WINDOW_BYTES / line.len());
        let runs: u32 = 50;
        let start = std::time::Instant::now();
        for _ in 0..runs {
            let captured = Captured::from_text("stderr", &raw);
            assert!(finish(&captured, &redaction()).is_some());
        }
        println!(
            "failure output: {:?} per {} KB window",
            start.elapsed() / runs,
            WINDOW_BYTES / 1024
        );
    }

    #[test]
    fn the_tail_buffer_keeps_the_last_window_of_a_stream() {
        let mut buf = TailBuffer::new();
        for _ in 0..100 {
            buf.push(&[b'a'; 1000]);
        }
        buf.push(b"\nerror: done\n");
        let captured = buf.into_captured("stderr");
        assert_eq!(captured.text.len(), WINDOW_BYTES);
        assert!(captured.text.ends_with("error: done\n"));
        assert_eq!(captured.dropped_bytes, 100_000 + 13 - WINDOW_BYTES as u64);
    }

    #[test]
    fn the_tail_buffer_does_not_keep_half_a_character() {
        let mut buf = TailBuffer::new();
        buf.push("é".repeat(WINDOW_BYTES).as_bytes());
        buf.push(b"x");
        let captured = buf.into_captured("stderr");
        assert!(
            !captured.text.contains('\u{fffd}'),
            "{:?}",
            &captured.text[..8]
        );
        assert!(captured.text.ends_with('x'));
    }
}
