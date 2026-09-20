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
