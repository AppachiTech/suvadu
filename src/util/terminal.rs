use std::sync::OnceLock;

// ── Color / TTY detection ──────────────────────────────

static COLOR_STDOUT: OnceLock<bool> = OnceLock::new();

/// Returns `true` if stdout is connected to a terminal (not piped/redirected).
/// Result is cached after the first call.
pub fn color_enabled() -> bool {
    *COLOR_STDOUT.get_or_init(|| {
        use std::io::IsTerminal;
        std::io::stdout().is_terminal()
    })
}

/// Returns `true` if stdin is connected to a terminal (interactive). Used to
/// refuse destructive y/N prompts when input is piped/redirected, where a
/// stray "y" could confirm something the caller never intended.
pub fn stdin_is_terminal() -> bool {
    use std::io::IsTerminal;
    std::io::stdin().is_terminal()
}

/// RAII guard that sets up and tears down the terminal for TUI rendering.
/// On creation it enters raw mode and the alternate screen.
/// On drop (including panic unwind) it restores the terminal.
pub struct TerminalGuard {
    terminal: ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>,
}

impl TerminalGuard {
    /// Enter raw mode + alternate screen and return a ready terminal.
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
        crossterm::terminal::enable_raw_mode()?;
        let mut stdout = std::io::stdout();
        crossterm::execute!(stdout, crossterm::terminal::EnterAlternateScreen)?;
        let backend = ratatui::backend::CrosstermBackend::new(stdout);
        let terminal = ratatui::Terminal::new(backend)?;
        Ok(Self { terminal })
    }

    /// Borrow the underlying terminal for rendering.
    pub const fn terminal(
        &mut self,
    ) -> &mut ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>> {
        &mut self.terminal
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
        let _ = crossterm::execute!(
            self.terminal.backend_mut(),
            crossterm::terminal::LeaveAlternateScreen
        );
        let _ = self.terminal.show_cursor();
    }
}

// ── Recall surface (PROD-09) ───────────────────────────────────

/// Where an interactive recall UI draws itself.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RecallSurface {
    /// Take over the screen. The default, and the full inspector.
    #[default]
    FullScreen,
    /// Draw in `height` rows under the prompt, leaving the scrollback — and
    /// therefore the surrounding shell context — on screen. Opt-in.
    Inline { height: u16 },
}

/// Smallest inline viewport worth drawing: query box, a few results, status
/// row and footer.
pub const INLINE_MIN_HEIGHT: u16 = 8;
/// Largest inline viewport. Beyond this it stops preserving context and may
/// as well be the full-screen inspector.
pub const INLINE_MAX_HEIGHT: u16 = 20;

/// The inline viewport height for a terminal of `terminal_rows` rows.
///
/// Always leaves rows above for the surrounding shell context — that is the
/// whole point of the inline surface — and never asks for more rows than the
/// terminal has.
pub fn inline_height(terminal_rows: u16) -> u16 {
    if terminal_rows <= INLINE_MIN_HEIGHT {
        // Too short to keep any context back; use what there is.
        return terminal_rows.max(1);
    }
    terminal_rows
        .saturating_sub(4)
        .clamp(INLINE_MIN_HEIGHT, INLINE_MAX_HEIGHT)
}

/// One terminal-mode change, named so setup/teardown can be checked without
/// a real terminal attached.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerminalStep {
    EnableRawMode,
    DisableRawMode,
    EnterAlternateScreen,
    LeaveAlternateScreen,
    EnableBracketedPaste,
    DisableBracketedPaste,
    /// Erase the inline viewport so the shell prompt returns to where it was.
    ClearInlineViewport,
    ShowCursor,
}

/// What entering `surface` changes about the terminal.
pub fn setup_steps(surface: RecallSurface) -> Vec<TerminalStep> {
    let mut steps = vec![TerminalStep::EnableRawMode];
    if surface == RecallSurface::FullScreen {
        steps.push(TerminalStep::EnterAlternateScreen);
    }
    steps.push(TerminalStep::EnableBracketedPaste);
    steps
}

/// What leaving `surface` must restore — the exact undo of [`setup_steps`],
/// in reverse, plus the cursor.
///
/// The inline surface never enters the alternate screen, so it must never
/// leave it either: doing so would wipe the scrollback it exists to preserve.
/// It erases its own viewport instead.
pub fn teardown_steps(surface: RecallSurface) -> Vec<TerminalStep> {
    let mut steps = vec![TerminalStep::DisableBracketedPaste];
    match surface {
        RecallSurface::FullScreen => steps.push(TerminalStep::LeaveAlternateScreen),
        RecallSurface::Inline { .. } => steps.push(TerminalStep::ClearInlineViewport),
    }
    steps.push(TerminalStep::DisableRawMode);
    steps.push(TerminalStep::ShowCursor);
    steps
}

/// Apply one [`TerminalStep`] to stderr.
fn apply_step(step: TerminalStep) -> std::io::Result<()> {
    use crossterm::{cursor, event, terminal};
    match step {
        TerminalStep::EnableRawMode => terminal::enable_raw_mode(),
        TerminalStep::DisableRawMode => terminal::disable_raw_mode(),
        TerminalStep::EnterAlternateScreen => {
            crossterm::execute!(std::io::stderr(), terminal::EnterAlternateScreen)
        }
        TerminalStep::LeaveAlternateScreen => {
            crossterm::execute!(std::io::stderr(), terminal::LeaveAlternateScreen)
        }
        TerminalStep::EnableBracketedPaste => {
            crossterm::execute!(std::io::stderr(), event::EnableBracketedPaste)
        }
        TerminalStep::DisableBracketedPaste => {
            crossterm::execute!(std::io::stderr(), event::DisableBracketedPaste)
        }
        // ratatui erases the inline viewport itself; this only makes sure
        // the cursor is back at the start of the reclaimed rows.
        TerminalStep::ClearInlineViewport => {
            crossterm::execute!(std::io::stderr(), cursor::MoveToColumn(0))
        }
        TerminalStep::ShowCursor => crossterm::execute!(std::io::stderr(), cursor::Show),
    }
}

/// RAII guard for stderr-based TUI (used by search, which needs stdout free for shell integration).
/// Restores terminal on drop, including panic unwind.
///
/// Setup and teardown run [`setup_steps`] and [`teardown_steps`] verbatim, so
/// the sequence the tests check is the sequence that runs.
pub struct TerminalGuardStderr {
    surface: RecallSurface,
}

impl TerminalGuardStderr {
    /// Enter raw mode + alternate screen + bracketed paste on stderr.
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
        Self::for_surface(RecallSurface::FullScreen)
    }

    /// Enter `surface`. Inline stays in the normal screen buffer so the
    /// commands already on screen remain visible.
    pub fn for_surface(surface: RecallSurface) -> Result<Self, Box<dyn std::error::Error>> {
        for step in setup_steps(surface) {
            apply_step(step)?;
        }
        Ok(Self { surface })
    }
}

impl Drop for TerminalGuardStderr {
    fn drop(&mut self) {
        // Every step is attempted even if an earlier one fails: leaving raw
        // mode on would make the shell unusable.
        for step in teardown_steps(self.surface) {
            let _ = apply_step(step);
        }
    }
}

#[cfg(test)]
mod cursor_report_tests {
    use super::parse_cursor_report;

    fn position(bytes: &[u8]) -> Option<(u16, u16)> {
        parse_cursor_report(bytes).map(|r| (r.row, r.col))
    }

    fn leftover(bytes: &[u8]) -> Vec<u8> {
        parse_cursor_report(bytes).expect("a report").leftover
    }

    #[test]
    fn reads_a_plain_report() {
        assert_eq!(position(b"\x1b[12;40R"), Some((12, 40)));
        assert!(leftover(b"\x1b[12;40R").is_empty());
    }

    #[test]
    fn reads_a_report_with_a_stray_keystroke_around_it() {
        assert_eq!(position(b"q\x1b[3;7Rx"), Some((3, 7)));
    }

    #[test]
    fn keeps_keystrokes_that_arrived_with_the_report() {
        // Typing during the measurement must not cost the typist those
        // characters: they are input, not part of the answer.
        assert_eq!(leftover(b"ab\x1b[3;7Rcd"), b"abcd".to_vec());
        assert_eq!(leftover(b"\x1b[3;7Rzzz"), b"zzz".to_vec());
        assert_eq!(leftover(b"zzz\x1b[3;7R"), b"zzz".to_vec());
    }

    #[test]
    fn takes_the_last_complete_report() {
        assert_eq!(position(b"\x1b[1;1R\x1b[9;2R"), Some((9, 2)));
    }

    #[test]
    fn rejects_an_incomplete_or_absent_report() {
        assert!(parse_cursor_report(b"").is_none());
        assert!(parse_cursor_report(b"\x1b[12;40").is_none());
        assert!(parse_cursor_report(b"hello").is_none());
    }
}

#[cfg(test)]
mod tests {
    use super::{
        inline_height, setup_steps, teardown_steps, RecallSurface, TerminalStep, INLINE_MAX_HEIGHT,
        INLINE_MIN_HEIGHT,
    };

    const INLINE: RecallSurface = RecallSurface::Inline { height: 12 };

    #[test]
    fn full_screen_is_the_default_surface() {
        assert_eq!(RecallSurface::default(), RecallSurface::FullScreen);
    }

    #[test]
    fn full_screen_teardown_undoes_exactly_what_setup_did() {
        let setup = setup_steps(RecallSurface::FullScreen);
        let teardown = teardown_steps(RecallSurface::FullScreen);
        assert_eq!(
            setup,
            vec![
                TerminalStep::EnableRawMode,
                TerminalStep::EnterAlternateScreen,
                TerminalStep::EnableBracketedPaste,
            ]
        );
        assert_eq!(
            teardown,
            vec![
                TerminalStep::DisableBracketedPaste,
                TerminalStep::LeaveAlternateScreen,
                TerminalStep::DisableRawMode,
                TerminalStep::ShowCursor,
            ]
        );
    }

    #[test]
    fn inline_never_touches_the_alternate_screen() {
        assert!(!setup_steps(INLINE).contains(&TerminalStep::EnterAlternateScreen));
        // Leaving a screen it never entered would erase the scrollback the
        // inline surface exists to preserve.
        assert!(!teardown_steps(INLINE).contains(&TerminalStep::LeaveAlternateScreen));
        assert!(teardown_steps(INLINE).contains(&TerminalStep::ClearInlineViewport));
    }

    #[test]
    fn every_surface_restores_raw_mode_paste_and_the_cursor() {
        for surface in [RecallSurface::FullScreen, INLINE] {
            let setup = setup_steps(surface);
            let teardown = teardown_steps(surface);
            assert!(setup.contains(&TerminalStep::EnableRawMode), "{surface:?}");
            assert!(
                setup.contains(&TerminalStep::EnableBracketedPaste),
                "{surface:?}"
            );
            assert!(
                teardown.contains(&TerminalStep::DisableRawMode),
                "{surface:?}: a left-behind raw mode makes the shell unusable"
            );
            assert!(
                teardown.contains(&TerminalStep::DisableBracketedPaste),
                "{surface:?}"
            );
            assert!(teardown.contains(&TerminalStep::ShowCursor), "{surface:?}");
        }
    }

    #[test]
    fn raw_mode_is_disabled_after_the_screen_is_restored() {
        for surface in [RecallSurface::FullScreen, INLINE] {
            let teardown = teardown_steps(surface);
            let raw = teardown
                .iter()
                .position(|s| *s == TerminalStep::DisableRawMode)
                .unwrap();
            let screen = teardown
                .iter()
                .position(|s| {
                    matches!(
                        s,
                        TerminalStep::LeaveAlternateScreen | TerminalStep::ClearInlineViewport
                    )
                })
                .unwrap();
            assert!(screen < raw, "{surface:?}: {teardown:?}");
        }
    }

    #[test]
    fn inline_height_leaves_room_for_the_surrounding_shell_context() {
        for rows in [24_u16, 30, 40, 60] {
            let h = inline_height(rows);
            assert!(h < rows, "{rows} rows: viewport {h} left no context");
            assert!(h <= INLINE_MAX_HEIGHT, "{rows} rows: viewport {h}");
        }
    }

    #[test]
    fn inline_height_never_exceeds_a_tiny_terminal() {
        for rows in [1_u16, 2, 5, 8] {
            let h = inline_height(rows);
            assert!(h >= 1, "{rows} rows");
            assert!(
                h <= rows,
                "{rows} rows: viewport {h} is taller than the terminal"
            );
        }
    }

    #[test]
    fn inline_height_is_capped_on_very_tall_terminals() {
        assert_eq!(inline_height(200), INLINE_MAX_HEIGHT);
        assert_eq!(inline_height(INLINE_MIN_HEIGHT), INLINE_MIN_HEIGHT);
    }
}

/// A terminal's reply to the cursor-position request, and anything that
/// arrived with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CursorReport {
    /// 1-based row, as the terminal reports it.
    pub row: u16,
    /// 1-based column, as the terminal reports it.
    pub col: u16,
    /// Bytes read from the terminal that were not part of the report —
    /// keystrokes typed while recall was measuring. They belong to whoever
    /// typed them, so they are handed back rather than dropped.
    pub leftover: Vec<u8>,
}

/// Parse a terminal's reply to the cursor-position request (`ESC[6n`),
/// which looks like `ESC[<row>;<col>R`, both 1-based.
///
/// The reply can arrive with other bytes around it: a keystroke typed at the
/// wrong moment, or a stray earlier response. Scanning for the last complete
/// report is more robust than assuming the buffer holds exactly one, and
/// everything outside it is returned in `leftover`.
pub fn parse_cursor_report(bytes: &[u8]) -> Option<CursorReport> {
    let text = String::from_utf8_lossy(bytes);
    let (start, body) = text
        .rmatch_indices("\u{1b}[")
        .find_map(|(i, _)| text[i + 2..].split_once('R').map(|(body, _)| (i, body)))?;
    let (row, col) = body.split_once(';')?;
    let row: u16 = row.trim().parse().ok()?;
    let col: u16 = col.trim().parse().ok()?;

    // `ESC[` + body + `R` is the report; the rest is someone's input.
    let end = start + 2 + body.len() + 1;
    let mut leftover = text[..start].as_bytes().to_vec();
    leftover.extend_from_slice(text[end..].as_bytes());
    Some(CursorReport { row, col, leftover })
}

/// Ask the controlling terminal where the cursor is.
///
/// `crossterm::cursor::position()` writes its request to **stdout**. Recall
/// draws to stderr precisely because stdout carries the selected command
/// back to the shell wrapper, which runs `suv search` inside a command
/// substitution — so on that path the request goes into a pipe, the terminal
/// never sees it, and the read times out with "The cursor position could not
/// be read within a normal duration". Asking `/dev/tty` directly keeps the
/// request and its reply on the terminal whatever stdout is connected to.
///
/// Raw mode must already be on, or the terminal will not answer. Returns an
/// error rather than a guess if it does not answer in time.
pub fn cursor_position_from_tty() -> std::io::Result<CursorReport> {
    use std::io::{Read, Write};
    use std::os::unix::io::AsRawFd;

    let mut tty = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/tty")?;
    tty.write_all(b"\x1b[6n")?;
    tty.flush()?;

    // Read without blocking, retrying until the deadline, rather than waiting
    // on readiness first. `poll` cannot be used here: on macOS it reports
    // POLLNVAL for terminal devices, which made the wait either hang on a
    // blocking read or give up before the reply arrived. Retrying a
    // non-blocking read needs no readiness call at all, so it behaves the
    // same on every platform.
    set_nonblocking(tty.as_raw_fd())?;

    let deadline = std::time::Instant::now() + CURSOR_REPORT_TIMEOUT;
    let mut buf = Vec::with_capacity(32);
    let mut chunk = [0u8; 32];
    while std::time::Instant::now() < deadline {
        let read = match tty.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => read,
            Err(err)
                if matches!(
                    err.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                ) =>
            {
                std::thread::sleep(CURSOR_REPORT_RETRY);
                continue;
            }
            Err(err) => return Err(err),
        };
        buf.extend_from_slice(&chunk[..read]);
        if let Some(report) = parse_cursor_report(&buf) {
            return Ok(report);
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::TimedOut,
        "the terminal did not report the cursor position",
    ))
}

/// How long to wait for the terminal's reply before giving up. Long enough
/// for a slow remote terminal, short enough not to look like a hang.
const CURSOR_REPORT_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(500);

/// How long to wait between attempts to read the reply. Short enough that
/// the terminal's answer is picked up without a visible pause.
const CURSOR_REPORT_RETRY: std::time::Duration = std::time::Duration::from_millis(1);

/// Switch `fd` to non-blocking reads.
///
/// The standard library has no portable way to do this for a `File`, so it
/// goes through `fcntl` directly.
#[allow(unsafe_code)]
fn set_nonblocking(fd: std::os::unix::io::RawFd) -> std::io::Result<()> {
    // SAFETY: `fcntl` takes the descriptor by value and touches no memory of
    // ours. `fd` belongs to a `File` the caller keeps open across both calls,
    // and each result is checked before use.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: as above; the flags were just read from this same descriptor.
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// A `CrosstermBackend` whose cursor query goes to the terminal rather than
/// to stdout.
///
/// Everything else is the crossterm backend unchanged; only
/// [`Backend::get_cursor_position`] differs, because that is the one call
/// crossterm routes through stdout (see [`cursor_position_from_tty`]).
pub struct TtyCursorBackend<W: std::io::Write> {
    inner: ratatui::backend::CrosstermBackend<W>,
    /// Keystrokes that arrived while the cursor was being measured. Reading
    /// from `/dev/tty` consumes them, so they are held here until the caller
    /// replays them into the UI.
    pending_input: Vec<u8>,
}

impl<W: std::io::Write> TtyCursorBackend<W> {
    pub const fn new(writer: W) -> Self {
        Self {
            inner: ratatui::backend::CrosstermBackend::new(writer),
            pending_input: Vec::new(),
        }
    }

    /// Take the keystrokes read alongside a cursor report, leaving none behind.
    pub fn take_pending_input(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.pending_input)
    }
}

impl<W: std::io::Write> ratatui::backend::Backend for TtyCursorBackend<W> {
    type Error = std::io::Error;

    fn draw<'a, I>(&mut self, content: I) -> std::io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a ratatui::buffer::Cell)>,
    {
        self.inner.draw(content)
    }

    fn hide_cursor(&mut self) -> std::io::Result<()> {
        self.inner.hide_cursor()
    }

    fn show_cursor(&mut self) -> std::io::Result<()> {
        self.inner.show_cursor()
    }

    fn get_cursor_position(&mut self) -> std::io::Result<ratatui::layout::Position> {
        let report = crate::util::cursor_position_from_tty()?;
        self.pending_input.extend_from_slice(&report.leftover);
        let (row, col) = (report.row, report.col);
        // The terminal reports 1-based row/column; ratatui wants 0-based x/y.
        Ok(ratatui::layout::Position {
            x: col.saturating_sub(1),
            y: row.saturating_sub(1),
        })
    }

    fn set_cursor_position<P: Into<ratatui::layout::Position>>(
        &mut self,
        position: P,
    ) -> std::io::Result<()> {
        self.inner.set_cursor_position(position)
    }

    fn clear(&mut self) -> std::io::Result<()> {
        self.inner.clear()
    }

    fn clear_region(&mut self, clear_type: ratatui::backend::ClearType) -> std::io::Result<()> {
        self.inner.clear_region(clear_type)
    }

    fn append_lines(&mut self, n: u16) -> std::io::Result<()> {
        self.inner.append_lines(n)
    }

    fn size(&self) -> std::io::Result<ratatui::layout::Size> {
        self.inner.size()
    }

    fn window_size(&mut self) -> std::io::Result<ratatui::backend::WindowSize> {
        self.inner.window_size()
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}
