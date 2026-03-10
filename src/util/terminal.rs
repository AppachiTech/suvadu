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

/// RAII guard for stderr-based TUI (used by search, which needs stdout free for shell integration).
/// Restores terminal on drop, including panic unwind.
pub struct TerminalGuardStderr;

impl TerminalGuardStderr {
    /// Enter raw mode + alternate screen on stderr.
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
        crossterm::terminal::enable_raw_mode()?;
        crossterm::execute!(std::io::stderr(), crossterm::terminal::EnterAlternateScreen)?;
        Ok(Self)
    }
}

impl Drop for TerminalGuardStderr {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
        let _ = crossterm::execute!(std::io::stderr(), crossterm::terminal::LeaveAlternateScreen);
    }
}

/// RAII guard for stdout-based TUI with mouse capture (used by session picker/timeline).
/// Restores terminal + disables mouse capture on drop.
pub struct TerminalGuardMouse;

impl TerminalGuardMouse {
    /// Enter raw mode + alternate screen + mouse capture on stdout.
    pub fn new() -> Result<Self, Box<dyn std::error::Error>> {
        crossterm::terminal::enable_raw_mode()?;
        crossterm::execute!(
            std::io::stdout(),
            crossterm::terminal::EnterAlternateScreen,
            crossterm::event::EnableMouseCapture
        )?;
        Ok(Self)
    }
}

impl Drop for TerminalGuardMouse {
    fn drop(&mut self) {
        let _ = crossterm::terminal::disable_raw_mode();
        let _ = crossterm::execute!(
            std::io::stdout(),
            crossterm::terminal::LeaveAlternateScreen,
            crossterm::event::DisableMouseCapture
        );
    }
}
